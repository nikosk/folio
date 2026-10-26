//! The document widget: a GtkScrollable that draws the engine's output with
//! GTK's GPU renderer and handles editing input, selection, links and
//! keyboard scrolling.

use crate::doc::Doc;
use crate::editor::{Heading, TableOp};
use crate::layout::{Engine, Img};
use crate::table::Align;
use crate::paint::{Painter, SnapPainter};
use crate::theme::{Theme, BODY_FONT};
use gtk::gdk::{self, Key, ModifierType};
use gtk::glib::{self, clone};
use gtk::subclass::prelude::*;
use gtk::{gio, pango, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::Duration;

type LinkHandler = Box<dyn Fn(&str)>;
type ChangeHandler = Box<dyn Fn()>;

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
        /// Whether Ctrl was held at the last key event (link hover cursor).
        pub cursor_ctrl: Cell<bool>,
        pub on_link: RefCell<Option<LinkHandler>>,
        pub on_change: RefCell<Option<ChangeHandler>>,
        pub first_frame: Cell<bool>,
        pub im: gtk::IMMulticontext,
        pub blink: RefCell<Option<glib::SourceId>>,
        pub version: Cell<u64>,
        pub on_scrolled: RefCell<Option<ChangeHandler>>,
        /// Right-click menu for tables.
        pub table_menu: RefCell<Option<gtk::PopoverMenu>>,
        /// The document's folder once it has been saved; pasted images go
        /// into `images/` under it.
        pub doc_dir: RefCell<Option<PathBuf>>,
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
                cursor_ctrl: Cell::new(false),
                on_link: RefCell::new(None),
                on_change: RefCell::new(None),
                first_frame: Cell::new(true),
                im: gtk::IMMulticontext::new(),
                blink: RefCell::new(None),
                version: Cell::new(0),
                on_scrolled: RefCell::new(None),
                table_menu: RefCell::new(None),
                doc_dir: RefCell::new(None),
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
            obj.setup_table_menu();
        }

        fn dispose(&self) {
            if let Some(m) = self.table_menu.take() {
                m.unparent();
            }
        }
    }

    impl WidgetImpl for DocView {
        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let obj = self.obj();
            if let Some(m) = self.table_menu.borrow().as_ref() {
                m.present();
            }
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
    pub fn set_doc(&self, src: String, doc: Doc, base_dir: PathBuf, keep_position: bool) {
        let imp = self.imp();
        let anchor = match (keep_position, imp.eng.borrow_mut().as_mut()) {
            (true, Some(e)) => Some(e.anchor(self.top())),
            _ => None,
        };
        let theme = imp.theme.borrow().clone();
        let mut e = Engine::new(self.pango_context(), src, doc, base_dir, theme, imp.zoom.get());
        e.set_width(self.width().max(1) as f64);
        // Keep already-decoded images across reloads.
        if let Some(old) = imp.eng.borrow_mut().as_mut() {
            e.images = std::mem::take(&mut old.images)
                .into_iter()
                .filter(|(_, v)| matches!(v, Img::Ready(_)))
                .collect();
        }
        *imp.eng.borrow_mut() = Some(e);
        imp.version.set(0);
        self.sync(Some(anchor.unwrap_or((0, 0.0))));
        self.queue_draw();
    }

    /// An empty, untitled document with the caret in it.
    pub fn set_empty(&self) {
        self.set_doc(String::new(), Doc::default(), std::env::current_dir().unwrap_or_default(), false);
        self.with_edit(|e| e.set_caret(0, false, true));
    }

    pub fn source(&self) -> Option<String> {
        self.imp().eng.borrow().as_ref().map(|e| e.src.clone())
    }

    pub fn is_dirty(&self) -> bool {
        self.imp().eng.borrow().as_ref().is_some_and(|e| e.is_dirty())
    }

    pub fn mark_saved(&self) {
        if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.mark_saved();
        }
        self.notify_change();
    }

    /// The saved document's folder (`None` while untitled).
    pub fn set_doc_dir(&self, dir: Option<PathBuf>) {
        *self.imp().doc_dir.borrow_mut() = dir;
    }

    pub fn set_base_dir(&self, dir: PathBuf) {
        if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.base_dir = dir;
        }
    }

    pub fn connect_change<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_change.borrow_mut() = Some(Box::new(f));
    }

    fn notify_change(&self) {
        if let Some(f) = self.imp().on_change.borrow().as_ref() {
            f();
        }
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
        if let Some(f) = self.imp().on_scrolled.borrow().as_ref() {
            f();
        }
    }

    pub fn connect_scrolled<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_scrolled.borrow_mut() = Some(Box::new(f));
    }

    // ---- outline -----------------------------------------------------------

    pub fn outline(&self) -> Vec<Heading> {
        self.imp().eng.borrow().as_ref().map(|e| e.outline()).unwrap_or_default()
    }

    /// Index into `outline()` of the section being read: the last heading
    /// at or above the top of the view (where outline clicks put it), else
    /// the first heading.
    pub fn current_heading(&self) -> Option<usize> {
        let top = self.top();
        let mut eng = self.imp().eng.borrow_mut();
        let e = eng.as_mut()?;
        let em = e.em();
        e.heading_at(top + 3.0 * em).or_else(|| e.heading_at(f64::MAX).map(|_| 0))
    }

    /// Scroll so the block at source offset `off` is near the top.
    pub fn scroll_to_off(&self, off: usize) {
        let y = self.imp().eng.borrow_mut().as_mut().and_then(|e| Some(e.off_y(off)? - e.em()));
        if let Some(y) = y {
            self.scroll_to(y);
        }
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
        let text = self.imp().eng.borrow().as_ref().map(|e| if e.editing() { e.selected_source() } else { e.selected_text() }).unwrap_or_default();
        if !text.is_empty() {
            self.clipboard().set_text(&text);
        }
    }

    pub fn select_all(&self) {
        if self.editing() {
            self.with_edit(|e| e.select_all_text());
        } else if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.select_all();
        }
        self.update_primary();
        self.queue_draw();
    }

    fn update_primary(&self) {
        let text = self.imp().eng.borrow().as_ref().map(|e| if e.editing() { e.selected_source() } else { e.selected_text() }).unwrap_or_default();
        if !text.is_empty() {
            self.primary_clipboard().set_text(&text);
        }
    }

    /// Source offset under a widget point.
    fn hit_off(&self, x: f64, y: f64) -> Option<usize> {
        let top = self.top();
        let mut eng = self.imp().eng.borrow_mut();
        let e = eng.as_mut()?;
        let (p, _) = e.hit(x, y + top)?;
        Some(e.pos_to_off(p))
    }

    // ---- editing -----------------------------------------------------------

    fn editing(&self) -> bool {
        self.imp().eng.borrow().as_ref().is_some_and(|e| e.editing())
    }

    /// Run an editing operation, then keep the caret in view, restart the
    /// blink and tell the window if the text changed.
    fn with_edit(&self, f: impl FnOnce(&mut Engine)) {
        let imp = self.imp();
        let version = {
            let mut eng = imp.eng.borrow_mut();
            let Some(e) = eng.as_mut() else { return };
            f(e);
            e.caret_on = true;
            e.ed.version
        };
        self.sync(None);
        self.scroll_to_caret();
        self.restart_blink();
        self.update_im_cursor();
        self.queue_draw();
        if imp.version.replace(version) != version {
            self.notify_change();
        }
    }

    fn scroll_to_caret(&self) {
        let h = self.height() as f64;
        let top = self.top();
        let rect = {
            let mut eng = self.imp().eng.borrow_mut();
            let Some(e) = eng.as_mut() else { return };
            let Some(p) = e.caret_pos() else { return };
            let em = e.em();
            e.caret_rect(p).map(|r| (r, em))
        };
        let Some(((_, y, ch), em)) = rect else { return };
        let margin = (1.5 * em).min(h / 4.0);
        if y < top + margin {
            self.scroll_to(y - margin);
        } else if y + ch > top + h - margin {
            self.scroll_to(y + ch + margin - h);
        }
    }

    fn update_im_cursor(&self) {
        let top = self.top();
        let rect = {
            let mut eng = self.imp().eng.borrow_mut();
            eng.as_mut().and_then(|e| e.caret_pos().and_then(|p| e.caret_rect(p)))
        };
        if let Some((x, y, h)) = rect {
            let r = gdk::Rectangle::new(x as i32, (y - top) as i32, 1, h as i32);
            self.imp().im.set_cursor_location(&r);
        }
    }

    fn restart_blink(&self) {
        let imp = self.imp();
        if let Some(id) = imp.blink.take() {
            id.remove();
        }
        let settings = gtk::Settings::default();
        let on = settings.as_ref().is_none_or(|s| s.is_gtk_cursor_blink());
        if !on || !self.editing() || !self.has_focus() {
            return;
        }
        let cycle = settings.as_ref().map_or(1200, |s| s.gtk_cursor_blink_time()).max(200) as u64;
        let timeout = settings.as_ref().map_or(10, |s| s.gtk_cursor_blink_timeout()).max(1) as u64;
        let ticks = Cell::new(0u64);
        let id = glib::timeout_add_local(Duration::from_millis(cycle / 2), clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                let n = ticks.get() + 1;
                ticks.set(n);
                // Stop blinking (caret shown) after the idle timeout, like GTK.
                let stop = n * cycle / 2 >= timeout * 1000;
                if let Some(e) = obj.imp().eng.borrow_mut().as_mut() {
                    e.caret_on = stop || !e.caret_on;
                }
                obj.queue_draw();
                if stop {
                    obj.imp().blink.take();
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            }
        ));
        *imp.blink.borrow_mut() = Some(id);
    }

    pub fn undo(&self) {
        self.with_edit(|e| {
            e.undo();
        });
    }

    pub fn redo(&self) {
        self.with_edit(|e| {
            e.redo();
        });
    }

    pub fn cut(&self) {
        let text = self.imp().eng.borrow().as_ref().map(|e| e.selected_source()).unwrap_or_default();
        if !text.is_empty() {
            self.clipboard().set_text(&text);
            self.with_edit(|e| {
                e.delete_selection();
            });
        }
    }

    /// Paste: files become links (images inline), text is inserted (a URL
    /// over a selection links it), and an image is saved under `images/`.
    pub fn paste(&self) {
        if !self.editing() {
            return;
        }
        let obj = self.downgrade();
        let clip = self.clipboard();
        glib::spawn_future_local(async move {
            let formats = clip.formats();
            if formats.contains_type(gdk::FileList::static_type()) {
                let v = clip.read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT).await;
                if let (Ok(v), Some(obj)) = (v, obj.upgrade())
                    && let Ok(list) = v.get::<gdk::FileList>()
                {
                    obj.insert_files(&list.files());
                }
                return;
            }
            if formats.contains_type(String::static_type()) || formats.contain_mime_type("text/plain") {
                if let (Ok(Some(text)), Some(obj)) = (clip.read_text_future().await, obj.upgrade()) {
                    obj.with_edit(|e| e.paste_text(&text));
                }
                return;
            }
            if let (Ok(Some(tex)), Some(obj)) = (clip.read_texture_future().await, obj.upgrade()) {
                obj.insert_image(&tex);
            }
        });
    }

    /// Link files at the caret: images as `![name](path)`, others as
    /// `[name](path)`, relative to the document's folder when inside it.
    pub fn insert_files(&self, files: &[gio::File]) {
        let dir = self.imp().doc_dir.borrow().clone();
        let links: Vec<String> = files
            .iter()
            .filter_map(|f| f.path())
            .map(|p| {
                let rel = dir.as_deref().and_then(|d| p.strip_prefix(d).ok()).unwrap_or(&p);
                let target = rel.to_string_lossy();
                let target = if target.contains([' ', '(', ')', '<', '>']) { format!("<{target}>") } else { target.into_owned() };
                let name = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if is_image(&p) { format!("![{name}]({target})") } else { format!("[{name}]({target})") }
            })
            .collect();
        if !links.is_empty() && self.editing() {
            self.with_edit(|e| e.insert(&links.join("\n")));
        }
    }

    /// Files dropped on the document: linked at the drop point.
    pub fn drop_files(&self, files: &[gio::File], x: f64, y: f64) {
        if !self.editing() {
            return;
        }
        if let Some(off) = self.hit_off(x, y) {
            self.with_edit(|e| e.set_caret(off, false, true));
        }
        self.insert_files(files);
    }

    /// Save a pasted image as `images/pasted-<time>.png` next to the document.
    fn insert_image(&self, tex: &gdk::Texture) {
        let Some(dir) = self.imp().doc_dir.borrow().clone() else {
            self.alert("Save the document first", "Pasted images are saved in an “images” folder next to the document.");
            return;
        };
        let images = dir.join("images");
        let stamp = glib::DateTime::now_local().ok().and_then(|d| d.format("%Y%m%d-%H%M%S").ok()).unwrap_or_default();
        let mut name = format!("pasted-{stamp}.png");
        let mut n = 2;
        while images.join(&name).exists() {
            name = format!("pasted-{stamp}-{n}.png");
            n += 1;
        }
        let path = images.join(&name);
        if let Err(e) = std::fs::create_dir_all(&images).map_err(|e| e.to_string()).and_then(|_| tex.save_to_png(&path).map_err(|e| e.to_string())) {
            self.alert("Could not save the pasted image", &format!("{}\n{e}", path.display()));
            return;
        }
        self.with_edit(|e| e.insert(&format!("![](images/{name})")));
    }

    fn alert(&self, message: &str, detail: &str) {
        let win = self.root().and_downcast::<gtk::Window>();
        gtk::AlertDialog::builder().message(message).detail(detail).build().show(win.as_ref());
    }

    /// Wrap the selection in `mark` (e.g. `**`), or insert a pair at the caret.
    fn wrap(&self, mark: &str) {
        self.with_edit(|e| {
            let sel = e.selection();
            let text = e.selected_source();
            let caret = e.ed.caret.unwrap_or(0);
            e.insert(&format!("{mark}{text}{mark}"));
            match sel {
                Some(r) => e.select_range(r.start + mark.len()..r.end + mark.len()),
                None => e.set_caret(caret + mark.len(), false, true),
            }
        });
    }

    fn insert_text(&self, text: &str) {
        if self.editing() {
            self.with_edit(|e| e.type_text(text));
        }
    }

    pub fn table_op(&self, op: TableOp) {
        self.with_edit(|e| {
            e.table_op(op);
        });
    }

    /// Row/column/alignment actions (`table.*`) and their right-click menu.
    fn setup_table_menu(&self) {
        let group = gio::SimpleActionGroup::new();
        let ops = [
            ("row-above", TableOp::RowAbove),
            ("row-below", TableOp::RowBelow),
            ("col-left", TableOp::ColLeft),
            ("col-right", TableOp::ColRight),
            ("del-row", TableOp::DelRow),
            ("del-col", TableOp::DelCol),
            ("align-left", TableOp::Align(Align::Left)),
            ("align-center", TableOp::Align(Align::Center)),
            ("align-right", TableOp::Align(Align::Right)),
        ];
        for (name, op) in ops {
            let a = gio::SimpleAction::new(name, None);
            a.connect_activate(clone!(
                #[weak(rename_to = obj)]
                self,
                move |_, _| obj.table_op(op)
            ));
            group.add_action(&a);
        }
        self.insert_action_group("table", Some(&group));
        let menu = gio::Menu::new();
        let section = |items: &[(&str, &str)]| {
            let m = gio::Menu::new();
            for (label, action) in items {
                m.append(Some(label), Some(&format!("table.{action}")));
            }
            m
        };
        menu.append_section(None, &section(&[("Insert Row Above", "row-above"), ("Insert Row Below", "row-below")]));
        menu.append_section(None, &section(&[("Insert Column Left", "col-left"), ("Insert Column Right", "col-right")]));
        menu.append_section(None, &section(&[("Delete Row", "del-row"), ("Delete Column", "del-col")]));
        menu.append_section(None, &section(&[("Align Left", "align-left"), ("Align Center", "align-center"), ("Align Right", "align-right")]));
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(self);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        *self.imp().table_menu.borrow_mut() = Some(pop);
    }

    /// Right click: put the caret there, and offer the table menu in tables.
    fn on_context(&self, x: f64, y: f64) {
        self.grab_focus();
        let in_table = match self.hit_off(x, y) {
            Some(off) => {
                let mut in_table = false;
                self.with_edit(|e| {
                    if e.selection().is_none_or(|r| !r.contains(&off)) {
                        e.set_caret(off, false, true);
                    }
                    in_table = e.in_table();
                });
                in_table
            }
            None => false,
        };
        if let (true, Some(m)) = (in_table, self.imp().table_menu.borrow().as_ref()) {
            m.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            m.popup();
        }
    }

    /// The key press, handled as an editing command. Returns false if unused.
    fn on_edit_key(&self, key: Key, state: ModifierType) -> bool {
        let ctrl = state.contains(ModifierType::CONTROL_MASK);
        let shift = state.contains(ModifierType::SHIFT_MASK);
        let page = self.height() as f64 * 0.9;
        match key {
            Key::z | Key::Z if ctrl && shift => self.redo(),
            Key::z if ctrl => self.undo(),
            Key::y if ctrl => self.redo(),
            Key::x if ctrl => self.cut(),
            Key::v if ctrl => self.paste(),
            Key::b if ctrl => self.wrap("**"),
            Key::i if ctrl => self.wrap("*"),
            Key::Left | Key::KP_Left => self.with_edit(|e| e.move_h(false, ctrl, shift)),
            Key::Right | Key::KP_Right => self.with_edit(|e| e.move_h(true, ctrl, shift)),
            Key::Up | Key::KP_Up => self.with_edit(|e| e.move_v(false, shift)),
            Key::Down | Key::KP_Down => self.with_edit(|e| e.move_v(true, shift)),
            Key::Home | Key::KP_Home if ctrl => self.with_edit(|e| e.set_caret(0, shift, true)),
            Key::End | Key::KP_End if ctrl => self.with_edit(|e| e.set_caret(e.src.len(), shift, true)),
            Key::Home | Key::KP_Home => self.with_edit(|e| e.move_line_edge(false, shift)),
            Key::End | Key::KP_End => self.with_edit(|e| e.move_line_edge(true, shift)),
            Key::Page_Up | Key::Page_Down | Key::KP_Page_Up | Key::KP_Page_Down => {
                let dy = if matches!(key, Key::Page_Up | Key::KP_Page_Up) { -page } else { page };
                self.scroll_by(dy);
                self.with_edit(|e| {
                    let Some(p) = e.caret_pos() else { return };
                    if let Some((x, y, h)) = e.caret_rect(p)
                        && let Some((t, _)) = e.hit(x, y + h / 2.0 + dy) {
                            let off = e.pos_to_off(t);
                            e.set_caret(off, shift, true);
                        }
                });
            }
            Key::BackSpace => self.with_edit(|e| e.backspace(ctrl)),
            Key::Delete | Key::KP_Delete => self.with_edit(|e| e.delete_forward(ctrl)),
            Key::Return | Key::KP_Enter | Key::ISO_Enter if ctrl => self.table_op(TableOp::RowBelow),
            Key::Return | Key::KP_Enter | Key::ISO_Enter => self.with_edit(|e| e.newline(shift)),
            Key::Tab | Key::KP_Tab => self.with_edit(|e| e.tab(shift)),
            Key::ISO_Left_Tab => self.with_edit(|e| e.tab(true)),
            Key::k if ctrl => self.with_edit(|e| e.insert_link()),
            Key::t if ctrl => self.with_edit(|e| e.insert_table()),
            Key::Escape => {
                self.with_edit(|e| e.clear_caret());
                self.restart_blink();
            }
            _ => return false,
        }
        if shift {
            self.update_primary();
        }
        true
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

        let context = gtk::GestureClick::new();
        context.set_button(gdk::BUTTON_SECONDARY);
        context.connect_pressed(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, _, x, y| obj.on_context(x, y)
        ));
        self.add_controller(context);

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
                if obj.imp().dragged.get() {
                    obj.with_edit(|e| e.settle());
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

        let im = &self.imp().im;
        im.set_client_widget(Some(self));
        im.connect_commit(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_, text| obj.insert_text(text)
        ));
        im.connect_preedit_changed(clone!(
            #[weak(rename_to = obj)]
            self,
            move |im| {
                let (text, _, _) = im.preedit_string();
                if let Some(e) = obj.imp().eng.borrow_mut().as_mut() {
                    e.preedit = text.to_string();
                }
                obj.queue_draw();
            }
        ));

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = obj)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |c, key, _, state| {
                // Text goes through the input method first (compose keys, IME).
                if obj.editing()
                    && let Some(ev) = c.current_event()
                    && obj.imp().im.filter_keypress(&ev)
                {
                    return glib::Propagation::Stop;
                }
                obj.on_key(key, state)
            }
        ));
        keys.connect_key_released(clone!(
            #[weak(rename_to = obj)]
            self,
            move |c, key, _, _| {
                if matches!(key, Key::Control_L | Key::Control_R) {
                    obj.imp().cursor_ctrl.set(false);
                }
                if obj.editing()
                    && let Some(ev) = c.current_event()
                {
                    obj.imp().im.filter_keypress(&ev);
                }
            }
        ));
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                obj.imp().im.focus_in();
                obj.restart_blink();
            }
        ));
        focus.connect_leave(clone!(
            #[weak(rename_to = obj)]
            self,
            move |_| {
                obj.imp().im.focus_out();
                if let Some(id) = obj.imp().blink.take() {
                    id.remove();
                }
                if let Some(e) = obj.imp().eng.borrow_mut().as_mut() {
                    e.caret_on = false;
                }
                obj.queue_draw();
            }
        ));
        self.add_controller(focus);

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
        let top = self.top();
        // Ctrl+click follows a link instead of placing the caret.
        if state.contains(ModifierType::CONTROL_MASK) {
            let link = imp.eng.borrow_mut().as_mut().and_then(|e| e.link_at(x, y + top));
            if let Some(url) = link {
                imp.clicks.set(0);
                self.follow_link(&url);
                return;
            }
        }
        let Some(off) = self.hit_off(x, y) else { return };
        let shift = state.contains(ModifierType::SHIFT_MASK);
        self.with_edit(|e| match n {
            1 => e.set_caret(off, shift, true),
            2 => e.select_word_at(off),
            _ => e.select_block_at(off),
        });
        if n > 1 || shift {
            self.update_primary();
        }
    }

    fn follow_link(&self, url: &str) {
        if let Some(anchor) = url.strip_prefix('#') {
            self.scroll_to_anchor(anchor);
        } else if let Some(f) = self.imp().on_link.borrow().as_ref() {
            f(url);
        }
    }

    fn on_drag(&self, x: f64, y: f64) {
        let imp = self.imp();
        if imp.clicks.get() == 0 {
            return;
        }
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
        let Some(off) = self.hit_off(x, y.clamp(0.0, h)) else { return };
        // The source view stays where it is while dragging, so the text
        // under the pointer doesn't move.
        if let Some(e) = self.imp().eng.borrow_mut().as_mut() {
            e.set_caret(off, true, false);
            e.caret_on = true;
        }
        self.queue_draw();
    }

    fn on_release(&self, _x: f64, _y: f64) {
        if self.imp().dragged.get() {
            self.update_primary();
        }
    }

    fn on_motion(&self, x: f64, y: f64) {
        let top = self.top();
        let ctrl = self.imp().cursor_ctrl.get();
        let kind = match self.imp().eng.borrow_mut().as_mut() {
            Some(e) => {
                // While editing, links open with Ctrl+click.
                if (ctrl || !e.editing()) && e.link_at(x, y + top).is_some() {
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
        // Ctrl+Alt+C is Copy Path.
        let alt = state.contains(ModifierType::ALT_MASK);
        self.imp().cursor_ctrl.set(ctrl || matches!(key, Key::Control_L | Key::Control_R));
        if self.editing() {
            if self.on_edit_key(key, state) {
                return glib::Propagation::Stop;
            }
            return match key {
                Key::c | Key::C | Key::Insert if ctrl && !alt => {
                    self.copy();
                    glib::Propagation::Stop
                }
                Key::a | Key::A if ctrl => {
                    self.select_all();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        let h = self.height() as f64;
        let line = 3.0 * 16.0 * self.zoom();
        match key {
            Key::c | Key::C | Key::Insert if ctrl && !alt => self.copy(),
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

pub fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif"))
}
