//! Mermaid diagrams: ```` ```mermaid ```` blocks rendered to SVG by the
//! `mermaid-svg` crate (pure Rust, under 0.1 ms per diagram, so layout can do
//! it inline and knows the size at once), then rasterized on a worker thread
//! through the system SVG loader, like other images.

use gtk::{gdk, gdk_pixbuf, gio, glib};
use std::hash::{Hash, Hasher};

pub fn is_mermaid(lang: &str) -> bool {
    lang.eq_ignore_ascii_case("mermaid")
}

pub struct Svg {
    pub svg: String,
    /// Natural size in CSS pixels.
    pub w: f64,
    pub h: f64,
}

/// Cache key for a diagram's source in a theme. Trailing whitespace is
/// ignored: the rendered block's text ends with a newline, the source view's
/// doesn't, and entering the block shouldn't rasterize it again.
pub fn key(src: &str, dark: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.trim_end().hash(&mut h);
    dark.hash(&mut h);
    h.finish()
}

/// Render `src` to SVG, or return the parse error.
pub fn render(src: &str, dark: bool) -> Result<Svg, String> {
    let theme = if dark { mermaid_svg::Theme::by_name("dark") } else { None }.unwrap_or_default();
    let svg = mermaid_svg::render_with(src, &theme).map_err(|e| {
        let e = e.to_string();
        e.strip_prefix("parse error: ").map_or(e.clone(), str::to_string)
    })?;
    let (w, h) = view_box(&svg).ok_or("diagram has no size")?;
    // The SVG says width="100%"; a rasterizer needs a real size.
    let svg = svg.replacen("width=\"100%\"", &format!("width=\"{w}\" height=\"{h}\""), 1);
    Ok(Svg { svg, w, h })
}

fn view_box(svg: &str) -> Option<(f64, f64)> {
    let head = &svg[..svg.find('>')?];
    let vb = head.split("viewBox=\"").nth(1)?.split('"').next()?;
    let v: Vec<f64> = vb.split([' ', ',']).filter_map(|n| n.parse().ok()).collect();
    (v.len() == 4 && v[2] > 0.0 && v[3] > 0.0).then(|| (v[2], v[3]))
}

/// Rasterize at `w`×`h` pixels (call on a worker thread).
pub fn rasterize(svg: &str, w: u32, h: u32) -> Option<gdk::Texture> {
    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(svg.as_bytes()));
    let pb = gdk_pixbuf::Pixbuf::from_stream_at_scale(&stream, w as i32, h as i32, true, gio::Cancellable::NONE).ok()?;
    #[allow(deprecated)]
    Some(gdk::Texture::for_pixbuf(&pb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_with_a_size_or_reports_errors() {
        let d = render("flowchart TD\n  A[Start] --> B[Done]", false).unwrap();
        assert!(d.w > 10.0 && d.h > 10.0, "{} x {}", d.w, d.h);
        assert!(d.svg.contains(&format!("width=\"{}\"", d.w)));
        assert!(!d.svg.contains("foreignObject"));
        assert!(render("this is not mermaid", false).is_err());
        assert_ne!(key("graph TD", false), key("graph TD", true));
    }
}
