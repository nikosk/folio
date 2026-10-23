//! Application shell: windows, actions, file loading and live reload.

use crate::doc::{self, Doc};
use crate::theme::Theme;
use crate::view::DocView;
use gtk::gdk;
use gtk::glib::{self, clone};
use gtk::{gio, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

const APP_ID: &str = "dev.folio.Folio";

type Preload = (PathBuf, JoinHandle<std::io::Result<Doc>>);

/// A document parsed on a worker thread while GTK initialises.
static PRELOAD: Mutex<Option<Preload>> = Mutex::new(None);

pub fn preload(path: &Path) {
    let Ok(path) = path.canonicalize() else { return };
    let p = path.clone();
    let handle = std::thread::spawn(move || std::fs::read_to_string(&p).map(|s| doc::parse(&s)));
    *PRELOAD.lock().unwrap() = Some((path, handle));
}

fn load(path: &Path) -> std::io::Result<Doc> {
    let pre = PRELOAD.lock().unwrap().take();
    if let Some((p, handle)) = pre
        && p == path {
            return handle.join().unwrap_or_else(|_| Err(std::io::Error::other("parser panicked")));
        }
    let bytes = std::fs::read(path)?;
    Ok(doc::parse(&String::from_utf8_lossy(&bytes)))
}

struct State {
    dark: Cell<bool>,
    css: gtk::CssProvider,
    windows: RefCell<Vec<Weak<Win>>>,
    gsettings: RefCell<Option<gio::Settings>>,
}

thread_local! {
    static STATE: RefCell<Option<Rc<State>>> = const { RefCell::new(None) };
}

fn state() -> Rc<State> {
    STATE.with(|s| s.borrow().clone().expect("app state"))
}

fn theme() -> Theme {
    if state().dark.get() { Theme::dark() } else { Theme::light() }
}

fn system_dark(settings: Option<&gio::Settings>) -> bool {
    if let Ok(t) = std::env::var("GTK_THEME") {
        return t.to_lowercase().contains("dark");
    }
    settings.is_some_and(|s| s.string("color-scheme") == "prefer-dark")
}

fn interface_settings() -> Option<gio::Settings> {
    let schema = gio::SettingsSchemaSource::default()?.lookup("org.gnome.desktop.interface", true)?;
    schema.has_key("color-scheme").then(|| gio::Settings::new("org.gnome.desktop.interface"))
}

fn apply_theme(dark: bool) {
    let st = state();
    st.dark.set(dark);
    let th = theme();
    if let Some(s) = gtk::Settings::default() {
        s.set_gtk_application_prefer_dark_theme(dark);
    }
    st.css.load_from_string(&th.css());
    for w in st.windows.borrow().iter().filter_map(Weak::upgrade) {
        w.view.set_theme(th.clone());
    }
}

pub fn run() -> glib::ExitCode {
    crate::mark("main");
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    app.connect_startup(startup);
    app.connect_activate(|app| {
        Win::new(app, None);
    });
    app.connect_open(|app, files, _| {
        for f in files {
            Win::new(app, f.path());
        }
    });
    app.run()
}

fn startup(app: &gtk::Application) {
    crate::mark("gtk initialised");
    let css = gtk::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    let settings = interface_settings();
    let dark = system_dark(settings.as_ref());
    if let Some(s) = &settings {
        s.connect_changed(Some("color-scheme"), |s, _| apply_theme(system_dark(Some(s))));
    }
    let st = Rc::new(State {
        dark: Cell::new(dark),
        css,
        windows: RefCell::new(Vec::new()),
        gsettings: RefCell::new(settings),
    });
    let _ = &st.gsettings;
    STATE.with(|s| *s.borrow_mut() = Some(st));
    apply_theme(dark);

    let toggle = gio::SimpleAction::new("toggle-theme", None);
    toggle.connect_activate(|_, _| apply_theme(!state().dark.get()));
    app.add_action(&toggle);
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&quit);

    for (action, accels) in [
        ("app.quit", &["<Ctrl>q"][..]),
        ("app.toggle-theme", &["<Ctrl>d"]),
        ("win.open", &["<Ctrl>o"]),
        ("win.close", &["<Ctrl>w"]),
        ("win.find", &["<Ctrl>f"]),
        ("win.find-next", &["<Ctrl>g", "F3"]),
        ("win.find-prev", &["<Ctrl><Shift>g", "<Shift>F3"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
        ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
        ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
        ("win.reload", &["<Ctrl>r", "F5"]),
        ("win.back", &["<Alt>Left"]),
    ] {
        app.set_accels_for_action(action, accels);
    }
}

struct Win {
    win: gtk::ApplicationWindow,
    view: DocView,
    title: gtk::Label,
    subtitle: gtk::Label,
    search: gtk::SearchBar,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    path: RefCell<Option<PathBuf>>,
    history: RefCell<Vec<PathBuf>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    reload_timer: RefCell<Option<glib::SourceId>>,
}

impl Win {
    fn new(app: &gtk::Application, path: Option<PathBuf>) -> Rc<Win> {
        crate::mark("building window");
        let win = gtk::ApplicationWindow::builder()
            .application(app)
            .default_width(940)
            .default_height(1000)
            .title("Folio")
            .build();
        win.add_css_class("folio");

        let title = gtk::Label::builder().label("Folio").ellipsize(gtk::pango::EllipsizeMode::Middle).build();
        title.add_css_class("title");
        let subtitle = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Start).visible(false).build();
        subtitle.add_css_class("subtitle");
        let titles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        titles.set_valign(gtk::Align::Center);
        titles.append(&title);
        titles.append(&subtitle);

        let header = gtk::HeaderBar::new();
        header.set_title_widget(Some(&titles));
        let open = gtk::Button::from_icon_name("document-open-symbolic");
        open.set_tooltip_text(Some("Open… (Ctrl+O)"));
        open.set_action_name(Some("win.open"));
        header.pack_start(&open);
        let theme_btn = gtk::Button::from_icon_name("weather-clear-night-symbolic");
        theme_btn.set_tooltip_text(Some("Toggle dark mode (Ctrl+D)"));
        theme_btn.set_action_name(Some("app.toggle-theme"));
        header.pack_end(&theme_btn);
        let find_btn = gtk::ToggleButton::new();
        find_btn.set_icon_name("edit-find-symbolic");
        find_btn.set_tooltip_text(Some("Find (Ctrl+F)"));
        header.pack_end(&find_btn);

        let entry = gtk::SearchEntry::new();
        entry.set_width_chars(34);
        let count = gtk::Label::new(None);
        count.add_css_class("match-count");
        let prev = gtk::Button::from_icon_name("go-up-symbolic");
        prev.set_action_name(Some("win.find-prev"));
        prev.set_tooltip_text(Some("Previous match (Shift+Enter)"));
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.set_action_name(Some("win.find-next"));
        next.set_tooltip_text(Some("Next match (Enter)"));
        let sbox = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        sbox.append(&entry);
        sbox.append(&count);
        sbox.append(&prev);
        sbox.append(&next);
        let search = gtk::SearchBar::builder().child(&sbox).show_close_button(true).build();
        search.connect_entry(&entry);
        find_btn.bind_property("active", &search, "search-mode-enabled").bidirectional().build();

        let view = DocView::default();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.append(&search);
        vbox.append(&scroller);
        win.set_titlebar(Some(&header));
        win.set_child(Some(&vbox));
        view.set_theme(theme());
        view.set_message("Open a Markdown file with Ctrl+O, or drop one here.");

        let w = Rc::new(Win {
            win,
            view,
            title,
            subtitle,
            search,
            entry,
            count,
            path: RefCell::new(None),
            history: RefCell::new(Vec::new()),
            monitor: RefCell::new(None),
            reload_timer: RefCell::new(None),
        });
        state().windows.borrow_mut().push(Rc::downgrade(&w));
        w.setup_actions();
        w.setup_search();
        w.setup_drop();
        w.setup_links();
        // The window owns the Rc through this handler and drops it on close.
        let keep = RefCell::new(Some(w.clone()));
        w.win.connect_destroy(move |_| {
            keep.borrow_mut().take();
        });
        if let Some(p) = path {
            w.open(p, false);
        }
        crate::mark("window built, document loaded");
        w.win.present();
        crate::mark("presented");
        w.view.grab_focus();
        if let Ok(out) = std::env::var("FOLIO_SCREENSHOT") {
            w.debug_screenshot(out);
        }
        w
    }

    /// Debug hook: FOLIO_SCREENSHOT=out.png [FOLIO_SCROLL=y] [FOLIO_FIND=text]
    /// renders the live window with GTK's renderer, saves it and exits.
    fn debug_screenshot(self: &Rc<Self>, out: String) {
        let w = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            if let Ok(q) = std::env::var("FOLIO_FIND") {
                w.search.set_search_mode(true);
                w.entry.set_text(&q);
            }
            if let Some(y) = std::env::var("FOLIO_SCROLL").ok().and_then(|y| y.parse().ok()) {
                w.view.scroll_to(y);
            }
            glib::timeout_add_local_once(Duration::from_millis(400), move || {
                let paintable = gtk::WidgetPaintable::new(Some(&w.win));
                let snap = gtk::Snapshot::new();
                paintable.snapshot(&snap, w.win.width() as f64, w.win.height() as f64);
                let renderer = w.win.renderer();
                if let (Some(node), Some(r)) = (snap.to_node(), renderer) {
                    let tex = r.render_texture(&node, None);
                    if let Err(e) = tex.save_to_png(&out) {
                        eprintln!("screenshot failed: {e}");
                    }
                }
                std::process::exit(0);
            });
        });
    }

    fn action(self: &Rc<Self>, name: &str, f: impl Fn(&Rc<Win>) + 'static) {
        let a = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        a.connect_activate(move |_, _| {
            if let Some(w) = weak.upgrade() {
                f(&w);
            }
        });
        self.win.add_action(&a);
    }

    fn setup_actions(self: &Rc<Self>) {
        self.action("open", |w| w.open_dialog());
        self.action("close", |w| w.win.close());
        self.action("find", |w| {
            w.search.set_search_mode(true);
            w.entry.grab_focus();
            w.entry.select_region(0, -1);
        });
        self.action("find-next", |w| w.find_step(true));
        self.action("find-prev", |w| w.find_step(false));
        self.action("zoom-in", |w| w.view.set_zoom(w.view.zoom() * 1.1));
        self.action("zoom-out", |w| w.view.set_zoom(w.view.zoom() / 1.1));
        self.action("zoom-reset", |w| w.view.set_zoom(1.0));
        self.action("reload", |w| w.reload());
        self.action("back", |w| {
            let prev = w.history.borrow_mut().pop();
            if let Some(p) = prev {
                w.open(p, false);
            }
        });
    }

    fn setup_search(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.entry.connect_search_changed(move |e| {
            if let Some(w) = weak.upgrade() {
                let n = w.view.find(&e.text());
                if n == 0 {
                    w.count.set_text(if e.text().is_empty() { "" } else { "No matches" });
                } else {
                    w.find_step(true);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(w) = weak.upgrade() {
                let shift = WidgetExt::display(&w.win)
                    .default_seat()
                    .and_then(|s| s.keyboard())
                    .is_some_and(|k| k.modifier_state().contains(gdk::ModifierType::SHIFT_MASK));
                w.find_step(!shift);
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_next_match(move |_| {
            if let Some(w) = weak.upgrade() {
                w.find_step(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_previous_match(move |_| {
            if let Some(w) = weak.upgrade() {
                w.find_step(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_mode_enabled_notify(move |s| {
            if let Some(w) = weak.upgrade()
                && !s.is_search_mode() {
                    w.view.find("");
                    w.count.set_text("");
                    w.view.grab_focus();
                }
        });
    }

    fn find_step(&self, forward: bool) {
        match self.view.find_step(forward) {
            Some((i, n)) => self.count.set_text(&format!("{i} of {n}")),
            None if !self.entry.text().is_empty() => self.count.set_text("No matches"),
            None => {}
        }
    }

    fn setup_drop(self: &Rc<Self>) {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let Some(w) = weak.upgrade() else { return false };
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            match list.files().first().and_then(|f| f.path()) {
                Some(p) => {
                    w.open(p, true);
                    true
                }
                None => false,
            }
        });
        self.win.add_controller(drop);
    }

    fn setup_links(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.view.connect_link(move |url| {
            if let Some(w) = weak.upgrade() {
                w.follow(url);
            }
        });
    }

    fn follow(self: &Rc<Self>, url: &str) {
        let has_scheme = url.split_once(':').is_some_and(|(s, _)| {
            s.len() > 1 && s.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        });
        if has_scheme && !url.starts_with("file:") {
            gtk::UriLauncher::new(url).launch(Some(&self.win), gio::Cancellable::NONE, |_| {});
            return;
        }
        let (target, frag) = url.split_once('#').unwrap_or((url, ""));
        let base = self.path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
        let path = crate::layout::resolve(&base, target);
        let is_md = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd" | "txt"));
        if is_md && path.is_file() {
            self.open(path, true);
            if !frag.is_empty() {
                self.view.scroll_to_anchor(frag);
            }
        } else if path.exists() {
            let uri = gio::File::for_path(&path).uri();
            gtk::UriLauncher::new(&uri).launch(Some(&self.win), gio::Cancellable::NONE, |_| {});
        }
    }

    fn open_dialog(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Markdown"));
        for s in ["md", "markdown", "mdown", "mkd", "mdx", "txt"] {
            filter.add_suffix(s);
        }
        filter.add_mime_type("text/markdown");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title("Open Markdown").filters(&filters).modal(true).build();
        if let Some(dir) = self.path.borrow().as_ref().and_then(|p| p.parent()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let weak = Rc::downgrade(self);
        dialog.open(Some(&self.win), gio::Cancellable::NONE, move |res| {
            if let (Some(w), Ok(file)) = (weak.upgrade(), res)
                && let Some(p) = file.path() {
                    w.open(p, true);
                }
        });
    }

    fn open(self: &Rc<Self>, path: PathBuf, push_history: bool) {
        let path = path.canonicalize().unwrap_or(path);
        let doc = match load(&path) {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("Could not open {}\n{e}", path.display());
                if self.view.has_doc() {
                    gtk::AlertDialog::builder().message("Could not open file").detail(msg).build().show(Some(&self.win));
                } else {
                    self.view.set_message(&msg);
                }
                return;
            }
        };
        let old = self.path.replace(Some(path.clone()));
        if push_history
            && let Some(old) = old.filter(|o| *o != path) {
                self.history.borrow_mut().push(old);
            }
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.view.set_doc(doc, base, false);
        self.update_titles(&path);
        self.watch(&path);
        self.refresh_search();
    }

    fn reload(self: &Rc<Self>) {
        let Some(path) = self.path.borrow().clone() else { return };
        // Mid-save files can briefly be missing or empty; keep the old view then.
        let Ok(bytes) = std::fs::read(&path) else { return };
        let doc = doc::parse(&String::from_utf8_lossy(&bytes));
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.view.set_doc(doc, base, true);
        self.refresh_search();
    }

    fn refresh_search(&self) {
        if self.search.is_search_mode() && !self.entry.text().is_empty() {
            let n = self.view.find(&self.entry.text());
            self.count.set_text(&if n == 0 { "No matches".into() } else { format!("{n} matches") });
        }
    }

    fn update_titles(&self, path: &Path) {
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
        self.title.set_text(&name);
        let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        let dir = match std::env::var("HOME") {
            Ok(home) if dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
            _ => dir,
        };
        self.subtitle.set_text(&dir);
        self.subtitle.set_visible(!dir.is_empty());
        self.win.set_title(Some(&format!("{name} — Folio")));
    }

    fn watch(self: &Rc<Self>, path: &Path) {
        let file = gio::File::for_path(path);
        let monitor = file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE).ok();
        if let Some(m) = &monitor {
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, ev| {
                use gio::FileMonitorEvent as E;
                if !matches!(ev, E::Changed | E::ChangesDoneHint | E::Created | E::MovedIn | E::Renamed) {
                    return;
                }
                let Some(w) = weak.upgrade() else { return };
                if let Some(id) = w.reload_timer.take() {
                    id.remove();
                }
                let weak = Rc::downgrade(&w);
                let id = glib::timeout_add_local_once(Duration::from_millis(60), move || {
                    if let Some(w) = weak.upgrade() {
                        w.reload_timer.take();
                        w.reload();
                    }
                });
                *w.reload_timer.borrow_mut() = Some(id);
            });
        }
        *self.monitor.borrow_mut() = monitor;
    }
}
