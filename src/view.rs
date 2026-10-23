//! The document widget: a GtkScrollable that draws the engine's output with
//! GTK's GPU renderer and handles selection, links and keyboard scrolling.

use crate::doc::Doc;
use crate::layout::{Engine, Img, Pos};
use crate::paint::{Painter, SnapPainter};
use crate::theme::{Theme, BODY_FONT};
use gtk::gdk::{self, Key, ModifierType};
use gtk::glib::{self, clone};
use gtk::subclass::prelude::*;
use gtk::{gio, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::time::Duration;

type LinkHandler = Box<dyn Fn(&str)>;

mod imp {
    use super::*;
    use std::sync::OnceLock;

    pub struct DocView {
        pub eng: RefCell<Option<Engine>>,
        pub theme: RefCell<Theme>,
        pub zoom: Cell<f64>,
        pub message: RefCell<String>,
        pub hadj: RefCell<Option<gtk::Adjustment>>,
        pub vadj: RefCell<Option<gtk::Adjustment>>,
        pub vadj_sig: RefCell<Option<glib::SignalHandlerId>>,
        pub busy: Cell<bool>,
        pub bg_running: Cell<bool>,
        pub press: Cell<Option<(f64, f64)>>,
        pub dragged: Cell<bool>,
        pub clicks: Cell<i32>,
        pub drag_pt: Cell<(f64, f64)>,
        pub autoscroll: RefCell<Option<glib::SourceId>>,
        pub cursor: Cell<u8>,
        pub on_link: RefCell<Option<LinkHandler>>,
        pub first_frame: Cell<bool>,
    }

    impl Default for DocView {
        fn default() -> Self {
            DocView {
                eng: RefCell::new(None),
                theme: RefCell::new(Theme::light()),
                zoom: Cell::new(1.0),
                message: RefCell::new(String::new()),
                hadj: RefCell::new(None),
                vadj: RefCell::new(None),
                vadj_sig: RefCell::new(None),
                busy: Cell::new(false),
                bg_running: Cell::new(false),
                press: Cell::new(None),
                dragged: Cell::new(false),
                clicks: Cell::new(0),
                drag_pt: Cell::new((0.0, 0.0)),
                autoscroll: RefCell::new(None),
                cursor: Cell::new(0),
                on_link: RefCell::new(None),
                first_frame: Cell::new(true),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DocView {
        const NAME: &'static str = "FolioDocView";
        type Type = super::DocView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);
    }

    impl ObjectImpl for DocView {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPS: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPS.get_or_init(|| {
                ["hadjustment", "vadjustment", "hscroll-policy", "vscroll-policy"]
                    .into_iter()
                    .map(glib::ParamSpecOverride::for_interface::<gtk::Scrollable>)
                    .collect()
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "hadjustment" => *self.hadj.borrow_mut() = value.get().ok().flatten(),
                "vadjustment" => self.obj().set_vadj(value.get().ok().flatten()),
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "hadjustment" => self.hadj.borrow().to_value(),
                "vadjustment" => self.vadj.borrow().to_value(),
                _ => gtk::ScrollablePolicy::Minimum.to_value(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.add_css_class("view");
            obj.setup_input();
        }
    }

    impl WidgetImpl for DocView {
        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let obj = self.obj();
            obj.relayout(|e| {
                e.set_width(width as f64);
            });
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if self.first_frame.get() {
                crate::mark("first snapshot");
            }
            let obj = self.obj();
            let (w, h) = (obj.width() as f64, obj.height() as f64);
            let top = obj.top();
            let mut p = SnapPainter(snapshot);
            let mut eng = self.eng.borrow_mut();
            let bg = eng.as_ref().map_or(self.theme.borrow().bg, |e| e.theme.bg);
            p.rect(0.0, 0.0, w, h, &bg);
            match eng.as_mut() {
                Some(e) => e.paint(&mut p, top, h),
                None => {
                    let th = self.theme.borrow();
                    let l = obj.create_pango_layout(Some(&self.message.borrow()));
                    let mut fd = pango::FontDescription::from_string(BODY_FONT);
                    fd.set_absolute_size(15.0 * self.zoom.get() * pango::SCALE as f64);
                    l.set_font_description(Some(&fd));
                    l.set_alignment(pango::Alignment::Center);
                    l.set_width(((w - 40.0).max(10.0) * pango::SCALE as f64) as i32);
                    let lh = l.pixel_size().1 as f64;
                    p.layout(&l, 20.0, ((h - lh) / 2.0).max(0.0), &th.muted);
                }
            }
            if self.first_frame.replace(false) && std::env::var_os("FOLIO_STARTUP").is_some() {
                crate::mark("document drawn");
                // Exit once the frame has actually been rendered and submitted.
                if let Some(clock) = obj.frame_clock() {
                    clock.connect_after_paint(|_| {
                        crate::mark("first frame rendered");
                        if let Ok(st) = std::fs::read_to_string("/proc/self/status")
                            && let Some(l) = st.lines().find(|l| l.starts_with("VmRSS")) {
                                eprintln!("{l}");
                            }
                        std::process::exit(0);
                    });
                }
            }
        }
    }

    impl ScrollableImpl for DocView {}
}

glib::wrapper! {
    pub struct DocView(ObjectSubclass<imp::DocView>)
        @extends gtk::Widget,
        @implements gtk::Scrollable, gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DocView {
    fn default() -> Self {
        glib::Object::new()
    }
}

const CURSOR_DEFAULT: u8 = 0;
const CURSOR_TEXT: u8 = 1;
const CURSOR_POINTER: u8 = 2;

impl DocView {
    pub fn set_message(&self, msg: &str) {
        *self.imp().message.borrow_mut() = msg.to_string();
        self.queue_draw();
    }

    /// Replace the document. With `keep_position`, the scroll position is
    /// carried over by block index (used for live reload).
    pub fn set_doc(&self, doc: Doc, base_dir: PathBuf, keep_position: bool) {
        let imp = self.imp();
        let anchor = match (keep_position, imp.eng.borrow_mut().as_mut()) {
            (true, Some(e)) => Some(e.anchor(self.top())),
            _ => None,
        };
        let theme = imp.theme.borrow().clone();
        let mut e = Engine::new(self.pango_context(), doc, base_dir, theme, imp.zoom.get());
        e.set_width(self.width().max(1) as f64);
        // Keep already-decoded images across reloads.
        if let Some(old) = imp.eng.borrow_mut().as_mut() {
            e.images = std::mem::take(&mut old.images)
                .into_iter()
                .filter(|(_, v)| matches!(v, Img::Ready(_)))
                .collect();
        }
        *imp.eng.borrow_mut() = Some(e);
        self.sync(Some(anchor.unwrap_or((0, 0.0))));
        self.queue_draw();
    }

    pub fn has_doc(&self) -> bool {
        self.imp().eng.borrow().is_some()
    }

    pub fn set_theme(&self, theme: Theme) {
        *self.imp().theme.borrow_mut() = theme.clone();
        if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.set_theme(theme);
        }
        self.queue_draw();
    }

    pub fn zoom(&self) -> f64 {
        self.imp().zoom.get()
    }

    pub fn set_zoom(&self, z: f64) {
        let z = (z * 20.0).round() / 20.0;
        let z = z.clamp(0.5, 3.0);
        self.imp().zoom.set(z);
        self.relayout(|e| e.set_zoom(z));
    }

    pub fn connect_link<F: Fn(&str) + 'static>(&self, f: F) {
        *self.imp().on_link.borrow_mut() = Some(Box::new(f));
    }

    fn top(&self) -> f64 {
        self.imp().vadj.borrow().as_ref().map_or(0.0, |a| a.value())
    }

    fn set_vadj(&self, adj: Option<gtk::Adjustment>) {
        let imp = self.imp();
        if let (Some(old), Some(id)) = (imp.vadj.borrow().as_ref(), imp.vadj_sig.take()) {
            old.disconnect(id);
        }
        if let Some(a) = &adj {
            let id = a.connect_value_changed(clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| obj.on_scroll()
            ));
            *imp.vadj_sig.borrow_mut() = Some(id);
        }
        *imp.vadj.borrow_mut() = adj;
        self.sync(None);
    }

    fn on_scroll(&self) {
        if self.imp().busy.get() {
            return;
        }
        self.sync(None);
        self.queue_draw();
    }

    /// Apply an engine change while keeping the content under the top edge fixed.
    fn relayout(&self, f: impl FnOnce(&mut Engine)) {
        let anchor = {
            let mut eng = self.imp().eng.borrow_mut();
            let Some(e) = eng.as_mut() else {
                drop(eng);
                self.sync(None);
                return;
            };
            let a = e.anchor(self.top());
            f(e);
            a
        };
        self.sync(Some(anchor));
        self.queue_draw();
    }

    /// Lay out the visible range and update the adjustment. With an anchor,
    /// the top edge is placed at that (block, fraction).
    fn sync(&self, anchor: Option<(usize, f64)>) {
        let imp = self.imp();
        let Some(vadj) = imp.vadj.borrow().clone() else { return };
        let h = self.height() as f64;
        let (top, total, em, done) = {
            let mut eng = imp.eng.borrow_mut();
            let Some(e) = eng.as_mut() else {
                drop(eng);
                imp.busy.set(true);
                vadj.configure(0.0, 0.0, h, 1.0, 1.0, h);
                imp.busy.set(false);
                return;
            };
            let mut top = anchor.map_or_else(|| vadj.value(), |a| e.anchor_top(a));
            e.ensure(top, top + h);
            if let Some(a) = anchor {
                top = e.anchor_top(a);
            }
            let total = e.total_height();
            (top.clamp(0.0, (total - h).max(0.0)), total, e.em(), e.fully_measured())
        };
        imp.busy.set(true);
        vadj.configure(top, 0.0, total.max(h), em * 3.0, h * 0.9, h);
        imp.busy.set(false);
        self.load_images();
        if !done && !imp.bg_running.get() {
            imp.bg_running.set(true);
            glib::idle_add_local(clone!(
                #[weak(rename_to = obj)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || obj.bg_measure()
            ));
        }
    }

    /// Refine off-screen heights in small time slices so the scrollbar
    /// becomes exact without ever blocking input.
    fn bg_measure(&self) -> glib::ControlFlow {
        let imp = self.imp();
        let before = self.top();
        let (anchor, done) = {
            let mut eng = imp.eng.borrow_mut();
            let Some(e) = eng.as_mut() else {
                imp.bg_running.set(false);
                return glib::ControlFlow::Break;
            };
            let a = e.anchor(before);
            (a, e.measure_some(Duration::from_millis(4)))
        };
        self.sync(Some(anchor));
        if (self.top() - before).abs() > 0.5 {
            self.queue_draw();
        }
        if done {
            imp.bg_running.set(false);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    }

    fn load_images(&self) {
        let reqs: Vec<PathBuf> = match self.imp().eng.borrow_mut().as_mut() {
            Some(e) => std::mem::take(&mut e.img_requests),
            None => return,
        };
        for path in reqs {
            let obj = self.downgrade();
            glib::spawn_future_local(async move {
                let p = path.clone();
                let tex = gio::spawn_blocking(move || gdk::Texture::from_filename(&p).ok()).await.ok().flatten();
                if let Some(obj) = obj.upgrade() {
                    let img = tex.map_or(Img::Failed, Img::Ready);
                    obj.relayout(|e| e.image_ready(&path, img));
                }
            });
        }
    }

    pub fn scroll_to(&self, y: f64) {
        if let Some(a) = self.imp().vadj.borrow().clone() {
            a.set_value(y.clamp(0.0, (a.upper() - a.page_size()).max(0.0)));
        }
    }

    fn scroll_by(&self, dy: f64) {
        self.scroll_to(self.top() + dy);
    }

    pub fn scroll_to_anchor(&self, anchor: &str) -> bool {
        let y = self.imp().eng.borrow_mut().as_mut().and_then(|e| e.anchor_y(anchor));
        if let Some(y) = y {
            self.scroll_to(y);
        }
        y.is_some()
    }

    // ---- find ------------------------------------------------------------

    pub fn find(&self, q: &str) -> usize {
        let n = self.imp().eng.borrow_mut().as_mut().map_or(0, |e| e.find(q));
        self.queue_draw();
        n
    }

    /// Step to the next/previous match; returns (1-based index, total).
    pub fn find_step(&self, forward: bool) -> Option<(usize, usize)> {
        let top = self.top();
        let h = self.height() as f64;
        let (y, cur, total) = {
            let mut eng = self.imp().eng.borrow_mut();
            let e = eng.as_mut()?;
            let pos = e.find_step(forward, top)?;
            (e.pos_y(pos), e.cur_match? + 1, e.matches.len())
        };
        if y < top + 0.1 * h || y > top + 0.85 * h {
            self.scroll_to(y - 0.3 * h);
        }
        self.queue_draw();
        Some((cur, total))
    }

    // ---- selection & clipboard -------------------------------------------

    pub fn copy(&self) {
        let text = self.imp().eng.borrow().as_ref().map(|e| e.selected_text()).unwrap_or_default();
        if !text.is_empty() {
            self.clipboard().set_text(&text);
        }
    }

    pub fn select_all(&self) {
        if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.select_all();
        }
        self.update_primary();
        self.queue_draw();
    }

    fn update_primary(&self) {
        let text = self.imp().eng.borrow().as_ref().map(|e| e.selected_text()).unwrap_or_default();
        if !text.is_empty() {
            self.primary_clipboard().set_text(&text);
        }
    }

    fn hit(&self, x: f64, y: f64) -> Option<Pos> {
        let top = self.top();
        self.imp().eng.borrow_mut().as_mut()?.hit(x, y + top).map(|h| h.0)
    }

    fn setup_input(&self) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_pressed(clone!(
            #[weak(rename_to = obj)]
            self,
            move |g, n, x, y| obj.on_press(n, x, y, g.current_event_state())
        ));
        click.connect_released(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, x, y| obj.on_release(x, y)
        ));
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        drag.connect_drag_update(clone!(
            #[weak(rename_to = obj)]
            self,
            move |g, dx, dy| {
                if let Some((sx, sy)) = g.start_point() {
                    obj.on_drag(sx + dx, sy + dy);
                }
            }
        ));
        drag.connect_drag_end(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, _| {
                if let Some(id) = obj.imp().autoscroll.take() {
                    id.remove();
                }
            }
        ));
        self.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, x, y| obj.on_motion(x, y)
        ));
        self.add_controller(motion);

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| obj.on_key(key, state)
        ));
        self.add_controller(keys);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |c, _, dy| {
                if c.current_event_state().contains(ModifierType::CONTROL_MASK) {
                    obj.set_zoom(obj.zoom() * if dy < 0.0 { 1.1 } else { 1.0 / 1.1 });
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.add_controller(scroll);
    }

    fn on_press(&self, n: i32, x: f64, y: f64, state: ModifierType) {
        self.grab_focus();
        let imp = self.imp();
        imp.press.set(Some((x, y)));
        imp.dragged.set(false);
        imp.clicks.set(n);
        let Some(pos) = self.hit(x, y) else { return };
        if let Some(e) = imp.eng.borrow_mut().as_mut() {
            e.sel = Some(match n {
                1 if state.contains(ModifierType::SHIFT_MASK) => (e.sel.map_or(pos, |s| s.0), pos),
                1 => (pos, pos),
                2 => e.word_at(pos),
                _ => e.block_range(pos),
            });
        }
        if n > 1 {
            self.update_primary();
        }
        self.queue_draw();
    }

    fn on_drag(&self, x: f64, y: f64) {
        let imp = self.imp();
        if let Some((px, py)) = imp.press.get()
            && (x - px).hypot(y - py) > 3.0 {
                imp.dragged.set(true);
            }
        if !imp.dragged.get() {
            return;
        }
        imp.drag_pt.set((x, y));
        self.extend_selection(x, y);
        let h = self.height() as f64;
        let outside = y < 0.0 || y > h;
        if outside && imp.autoscroll.borrow().is_none() {
            let id = glib::timeout_add_local(Duration::from_millis(16), clone!(
                #[weak(rename_to = obj)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    let (x, y) = obj.imp().drag_pt.get();
                    let h = obj.height() as f64;
                    let over = if y < 0.0 { y } else if y > h { y - h } else { 0.0 };
                    if over == 0.0 {
                        obj.imp().autoscroll.take();
                        return glib::ControlFlow::Break;
                    }
                    obj.scroll_by(over.clamp(-60.0, 60.0) * 0.5);
                    obj.extend_selection(x, y);
                    glib::ControlFlow::Continue
                }
            ));
            *imp.autoscroll.borrow_mut() = Some(id);
        }
    }

    fn extend_selection(&self, x: f64, y: f64) {
        let h = self.height() as f64;
        let Some(pos) = self.hit(x, y.clamp(0.0, h)) else { return };
        if let Some(e) = self.imp().eng.borrow_mut().as_mut()
            && let Some(sel) = &mut e.sel {
                sel.1 = pos;
            }
        self.queue_draw();
    }

    fn on_release(&self, x: f64, y: f64) {
        let imp = self.imp();
        if imp.dragged.get() {
            self.update_primary();
            return;
        }
        if imp.clicks.get() != 1 {
            return;
        }
        let top = self.top();
        let link = {
            let mut eng = imp.eng.borrow_mut();
            let Some(e) = eng.as_mut() else { return };
            if e.sel.is_some_and(|(a, b)| a == b) {
                e.sel = None;
            }
            e.link_at(x, y + top)
        };
        self.queue_draw();
        if let Some(url) = link {
            if let Some(anchor) = url.strip_prefix('#') {
                self.scroll_to_anchor(anchor);
            } else if let Some(f) = imp.on_link.borrow().as_ref() {
                f(&url);
            }
        }
    }

    fn on_motion(&self, x: f64, y: f64) {
        let top = self.top();
        let kind = match self.imp().eng.borrow_mut().as_mut() {
            Some(e) => {
                if e.link_at(x, y + top).is_some() {
                    CURSOR_POINTER
                } else if e.over_text(x, y + top) {
                    CURSOR_TEXT
                } else {
                    CURSOR_DEFAULT
                }
            }
            None => CURSOR_DEFAULT,
        };
        if self.imp().cursor.replace(kind) != kind {
            self.set_cursor_from_name(match kind {
                CURSOR_POINTER => Some("pointer"),
                CURSOR_TEXT => Some("text"),
                _ => None,
            });
        }
    }

    fn on_key(&self, key: Key, state: ModifierType) -> glib::Propagation {
        let ctrl = state.contains(ModifierType::CONTROL_MASK);
        let shift = state.contains(ModifierType::SHIFT_MASK);
        let h = self.height() as f64;
        let line = 3.0 * 16.0 * self.zoom();
        match key {
            Key::c | Key::C | Key::Insert if ctrl => self.copy(),
            Key::a | Key::A if ctrl => self.select_all(),
            Key::Up | Key::k => self.scroll_by(-line),
            Key::Down | Key::j => self.scroll_by(line),
            Key::Page_Up => self.scroll_by(-0.9 * h),
            Key::Page_Down => self.scroll_by(0.9 * h),
            Key::space => self.scroll_by(if shift { -0.9 } else { 0.9 } * h),
            Key::Home | Key::g if !shift => self.scroll_to(0.0),
            Key::End | Key::G => self.scroll_to(f64::MAX),
            Key::Escape => {
                if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
                    e.sel = None;
                }
                self.queue_draw();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }
}
