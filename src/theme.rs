use crate::doc::Alert;
use crate::highlight::Tok;
use gtk::gdk::RGBA;

pub const BODY_FONT: &str = "Inter, Ubuntu Sans, Noto Sans, Cantarell, DejaVu Sans, sans-serif";
pub const MONO_FONT: &str =
    "JetBrains Mono, Fira Code, Cascadia Code, Ubuntu Sans Mono, Ubuntu Mono, DejaVu Sans Mono, monospace";
pub const MATH_FONT: &str = "Latin Modern Math, STIX Two Math, Noto Serif, DejaVu Serif, serif";

#[derive(Clone)]
pub struct Theme {
    pub dark: bool,
    pub bg: RGBA,
    pub fg: RGBA,
    pub heading: RGBA,
    pub muted: RGBA,
    pub link: RGBA,
    pub border: RGBA,
    pub code_bg: RGBA,
    pub inline_code_bg: RGBA,
    pub quote_bar: RGBA,
    pub sel: RGBA,
    pub find: RGBA,
    pub find_cur: RGBA,
    pub table_head: RGBA,
    pub stripe: RGBA,
    pub accent: RGBA,
    pub on_accent: RGBA,
    syn: [RGBA; 9],
    alerts: [RGBA; 5],
}

fn c(hex: u32, a: f32) -> RGBA {
    RGBA::new(
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
        a,
    )
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            bg: c(0xffffff, 1.0),
            fg: c(0x2b2f36, 1.0),
            heading: c(0x15181d, 1.0),
            muted: c(0x6a737d, 1.0),
            link: c(0x0b69d6, 1.0),
            border: c(0xe3e6ea, 1.0),
            code_bg: c(0xf6f8fa, 1.0),
            inline_code_bg: c(0xafb8c1, 0.22),
            quote_bar: c(0xdde1e6, 1.0),
            sel: c(0x0b69d6, 0.20),
            find: c(0xffd43b, 0.45),
            find_cur: c(0xff8c1a, 0.55),
            table_head: c(0xf6f8fa, 1.0),
            stripe: c(0xfafbfc, 1.0),
            accent: c(0x0b69d6, 1.0),
            on_accent: c(0xffffff, 1.0),
            syn: [
                c(0xcf222e, 1.0),
                c(0x0a3069, 1.0),
                c(0x6e7781, 1.0),
                c(0x0550ae, 1.0),
                c(0x953800, 1.0),
                c(0x8250df, 1.0),
                c(0x116329, 1.0),
                c(0x82071e, 1.0),
                c(0x6639ba, 1.0),
            ],
            alerts: [c(0x0969da, 1.0), c(0x1a7f37, 1.0), c(0x8250df, 1.0), c(0x9a6700, 1.0), c(0xd1242f, 1.0)],
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            bg: c(0x1c1e22, 1.0),
            fg: c(0xd3d7dd, 1.0),
            heading: c(0xf0f2f5, 1.0),
            muted: c(0x8b929b, 1.0),
            link: c(0x6cb0ff, 1.0),
            border: c(0x33373e, 1.0),
            code_bg: c(0x24272c, 1.0),
            inline_code_bg: c(0x6e7681, 0.25),
            quote_bar: c(0x3c414a, 1.0),
            sel: c(0x6cb0ff, 0.28),
            find: c(0xe0b400, 0.35),
            find_cur: c(0xff8c1a, 0.5),
            table_head: c(0x24272c, 1.0),
            stripe: c(0x202226, 1.0),
            accent: c(0x5aa2f5, 1.0),
            on_accent: c(0x101214, 1.0),
            syn: [
                c(0xff7b72, 1.0),
                c(0xa5d6ff, 1.0),
                c(0x8b949e, 1.0),
                c(0x79c0ff, 1.0),
                c(0xffa657, 1.0),
                c(0xd2a8ff, 1.0),
                c(0x7ee787, 1.0),
                c(0xffa198, 1.0),
                c(0xd2a8ff, 1.0),
            ],
            alerts: [c(0x4493f8, 1.0), c(0x3fb950, 1.0), c(0xab7df8, 1.0), c(0xd29922, 1.0), c(0xf85149, 1.0)],
        }
    }

    pub fn syntax(&self, t: Tok) -> RGBA {
        self.syn[t as usize]
    }

    pub fn alert(&self, a: Alert) -> RGBA {
        self.alerts[a as usize]
    }

    /// CSS for the window chrome so it blends with the document.
    pub fn css(&self) -> String {
        let hex = |c: &RGBA| {
            format!(
                "rgba({},{},{},{})",
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8,
                c.alpha()
            )
        };
        format!(
            "window.folio, window.folio .view {{ background: {bg}; }}
window.folio headerbar {{ background: {bg}; box-shadow: none; border-bottom: 1px solid {border}; min-height: 40px; }}
window.folio headerbar .title {{ font-weight: 600; }}
window.folio headerbar .subtitle {{ font-size: 0.82em; color: {muted}; }}
window.folio searchbar > revealer > box {{ background: {bg}; border-bottom: 1px solid {border}; }}
window.folio .match-count {{ color: {muted}; font-size: 0.9em; }}",
            bg = hex(&self.bg),
            border = hex(&self.border),
            muted = hex(&self.muted),
        )
    }
}
