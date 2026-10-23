//! Drawing backend abstraction: GTK snapshots (GPU) in the app, Cairo for
//! headless PNG rendering used in tests and benchmarks.

use gtk::gdk::RGBA;
use gtk::{gdk, graphene, gsk, pango};
use gtk::prelude::*;

#[allow(clippy::too_many_arguments)]
pub trait Painter {
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: &RGBA);
    fn rrect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, c: &RGBA);
    fn rborder(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, width: f64, c: &RGBA);
    fn layout(&mut self, l: &pango::Layout, x: f64, y: f64, c: &RGBA);
    fn texture(&mut self, t: &gdk::Texture, x: f64, y: f64, w: f64, h: f64);
    /// A straight stroke with round caps.
    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, c: &RGBA);
}

fn grect(x: f64, y: f64, w: f64, h: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, w as f32, h as f32)
}

pub struct SnapPainter<'a>(pub &'a gtk::Snapshot);

#[allow(clippy::too_many_arguments)]
impl Painter for SnapPainter<'_> {
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: &RGBA) {
        self.0.append_color(c, &grect(x, y, w, h));
    }

    fn rrect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, c: &RGBA) {
        let rect = grect(x, y, w, h);
        self.0.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, r as f32));
        self.0.append_color(c, &rect);
        self.0.pop();
    }

    fn rborder(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, width: f64, c: &RGBA) {
        let rr = gsk::RoundedRect::from_rect(grect(x, y, w, h), r as f32);
        let wd = width as f32;
        self.0.append_border(&rr, &[wd; 4], &[*c; 4]);
    }

    fn layout(&mut self, l: &pango::Layout, x: f64, y: f64, c: &RGBA) {
        self.0.save();
        self.0.translate(&graphene::Point::new(x as f32, y as f32));
        self.0.append_layout(l, c);
        self.0.restore();
    }

    fn texture(&mut self, t: &gdk::Texture, x: f64, y: f64, w: f64, h: f64) {
        self.0.append_scaled_texture(t, gsk::ScalingFilter::Trilinear, &grect(x, y, w, h));
    }

    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, c: &RGBA) {
        let (dx, dy) = (x2 - x1, y2 - y1);
        let len = dx.hypot(dy);
        let s = self.0;
        s.save();
        s.translate(&graphene::Point::new(x1 as f32, y1 as f32));
        s.rotate(dy.atan2(dx).to_degrees() as f32);
        let r = width / 2.0;
        let rect = grect(-r, -r, len + width, width);
        s.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, r as f32));
        s.append_color(c, &rect);
        s.pop();
        s.restore();
    }
}

pub struct CairoPainter<'a>(pub &'a cairo::Context);

impl CairoPainter<'_> {
    fn set(&self, c: &RGBA) {
        self.0.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
    }

    fn path(&self, x: f64, y: f64, w: f64, h: f64, r: f64) {
        let cr = self.0;
        let r = r.min(w / 2.0).min(h / 2.0);
        use std::f64::consts::{FRAC_PI_2, PI};
        cr.new_sub_path();
        cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
        cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
        cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
        cr.arc(x + r, y + r, r, PI, 3.0 * FRAC_PI_2);
        cr.close_path();
    }
}

#[allow(clippy::too_many_arguments)]
impl Painter for CairoPainter<'_> {
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: &RGBA) {
        self.set(c);
        self.0.rectangle(x, y, w, h);
        let _ = self.0.fill();
    }

    fn rrect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, c: &RGBA) {
        self.set(c);
        self.path(x, y, w, h, r);
        let _ = self.0.fill();
    }

    fn rborder(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, width: f64, c: &RGBA) {
        self.set(c);
        let i = width / 2.0;
        self.path(x + i, y + i, w - width, h - width, (r - i).max(0.0));
        self.0.set_line_width(width);
        let _ = self.0.stroke();
    }

    fn layout(&mut self, l: &pango::Layout, x: f64, y: f64, c: &RGBA) {
        self.set(c);
        self.0.move_to(x, y);
        pangocairo::functions::show_layout(self.0, l);
    }

    fn texture(&mut self, t: &gdk::Texture, x: f64, y: f64, w: f64, h: f64) {
        let (tw, th) = (t.width(), t.height());
        let stride = tw as usize * 4;
        let mut data = vec![0u8; stride * th as usize];
        t.download(&mut data, stride);
        let Ok(surf) = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, tw, th, stride as i32) else {
            return;
        };
        let cr = self.0;
        cr.save().ok();
        cr.translate(x, y);
        cr.scale(w / tw as f64, h / th as f64);
        let _ = cr.set_source_surface(&surf, 0.0, 0.0);
        cr.source().set_filter(cairo::Filter::Good);
        let _ = cr.paint();
        cr.restore().ok();
    }

    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, c: &RGBA) {
        self.set(c);
        self.0.set_line_width(width);
        self.0.set_line_cap(cairo::LineCap::Round);
        self.0.move_to(x1, y1);
        self.0.line_to(x2, y2);
        let _ = self.0.stroke();
    }
}
