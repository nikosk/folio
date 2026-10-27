//! Application shell: windows, actions, file loading and live reload.

use crate::doc::{self, Doc};
use crate::files::{self, is_markdown, FileTree, QuickOpen};
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

type Preload = (PathBuf, JoinHandle<std::io::Result<(String, Doc)>>);

/// A document parsed on a worker thread while GTK initialises.
static PRELOAD: Mutex<Option<Preload>> = Mutex::new(None);

pub fn preload(path: &Path) {
    let Ok(path) = path.canonicalize() else { return };
    let p = path.clone();
    let handle = std::thread::spawn(move || read_doc(&p));
    *PRELOAD.lock().unwrap() = Some((path, handle));
}

fn read_doc(path: &Path) -> std::io::Result<(String, Doc)> {
    let bytes = std::fs::read(path)?;
    let src = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    let doc = doc::parse(&src);
    Ok((src, doc))
}

fn load(path: &Path) -> std::io::Result<(String, Doc)> {
    let pre = PRELOAD.lock().unwrap().take();
    if let Some((p, handle)) = pre
        && p == path {
            return handle.join().unwrap_or_else(|_| Err(std::io::Error::other("parser panicked")));
        }
    read_doc(path)
}

struct State {
    dark: Cell<bool>,
    /// Sidebar shown, and its page ("outline" or "files"); new windows
    /// follow the last choice.
    sidebar: Cell<bool>,
    side_page: RefCell<String>,
    spell: Cell<bool>,
    /// Show hidden files in the files sidebar and go to file.
    hidden: Cell<bool>,
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

/// Preferences kept between runs, as `key=value` lines in
/// `~/.config/folio/settings`.
fn settings_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("folio").join("settings"))
}

fn load_settings() -> Vec<(String, String)> {
    let text = settings_file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default();
    text.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect()
}

fn save_settings() {
    let st = state();
    let text = format!(
        "spell={}\nsidebar={}\nside_page={}\nshow_hidden={}\n",
        st.spell.get(),
        st.sidebar.get(),
        st.side_page.borrow(),
        st.hidden.get()
    );
    if let Some(f) = settings_file() {
        let _ = std::fs::create_dir_all(f.parent().unwrap_or(&f)).and_then(|_| std::fs::write(&f, text));
    }
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
    let saved = load_settings();
    let get = |k: &str| saved.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    let st = Rc::new(State {
        dark: Cell::new(dark),
        sidebar: Cell::new(get("sidebar") == Some("true")),
        side_page: RefCell::new(get("side_page").filter(|p| *p == "files").unwrap_or("outline").into()),
        spell: Cell::new(get("spell") != Some("false")),
        hidden: Cell::new(get("show_hidden") == Some("true")),
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
    // Closing each window gives unsaved documents a chance to be saved.
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(|_, _| {
        for w in state().windows.borrow().iter().filter_map(Weak::upgrade) {
            w.win.close();
        }
    });
    app.add_action(&quit);
    let new = gio::SimpleAction::new("new", None);
    new.connect_activate(clone!(
        #[weak]
        app,
        move |_, _| {
            Win::new(&app, None);
        }
    ));
    app.add_action(&new);
    let spell = gio::SimpleAction::new_stateful("spell", None, &state().spell.get().to_variant());
    spell.connect_activate(|a, _| {
        let on = !state().spell.get();
        state().spell.set(on);
        save_settings();
        a.set_state(&on.to_variant());
        for w in state().windows.borrow().iter().filter_map(Weak::upgrade) {
            w.view.set_spell(on);
        }
    });
    app.add_action(&spell);
    let hidden = gio::SimpleAction::new_stateful("show-hidden", None, &state().hidden.get().to_variant());
    hidden.connect_activate(|a, _| {
        let on = !state().hidden.get();
        state().hidden.set(on);
        save_settings();
        a.set_state(&on.to_variant());
        for w in state().windows.borrow().iter().filter_map(Weak::upgrade) {
            w.files.set_show_hidden(on);
        }
    });
    app.add_action(&hidden);

    for (action, accels) in [
        ("app.quit", &["<Ctrl>q"][..]),
        ("app.toggle-theme", &["<Ctrl>d"]),
        ("app.spell", &["F7"]),
        ("app.show-hidden", &["<Ctrl>h"]),
        ("win.sidebar", &["F9"]),
        ("win.quick-open", &["<Ctrl>p"]),
        ("app.new", &["<Ctrl>n"]),
        ("win.open", &["<Ctrl>o"]),
        ("win.save", &["<Ctrl>s"]),
        ("win.save-as", &["<Ctrl><Shift>s"]),
        ("win.close", &["<Ctrl>w"]),
        ("win.find", &["<Ctrl>f"]),
        ("win.find-next", &["<Ctrl>g", "F3"]),
        ("win.find-prev", &["<Ctrl><Shift>g", "<Shift>F3"]),
        ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal", "<Ctrl>KP_Add"]),
        ("win.zoom-out", &["<Ctrl>minus", "<Ctrl>KP_Subtract"]),
        ("win.zoom-reset", &["<Ctrl>0", "<Ctrl>KP_0"]),
        ("win.reload", &["<Ctrl>r", "F5"]),
        ("win.back", &["<Alt>Left"]),
        ("win.copy-path", &["<Ctrl><Alt>c"]),
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
    /// Set once the user chose to close despite unsaved changes.
    closing: Cell<bool>,
    side_btn: gtk::ToggleButton,
    side_stack: gtk::Stack,
    outline_list: gtk::ListBox,
    files: Rc<FileTree>,
    quick: Rc<QuickOpen>,
    /// The folder the files sidebar and quick open show.
    root: RefCell<Option<PathBuf>>,
    /// The headings the list shows: (level, text, source offset).
    outline: RefCell<Vec<(u8, String, usize)>>,
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
        let file_menu = gio::Menu::new();
        file_menu.append(Some("Open…"), Some("win.open"));
        file_menu.append(Some("Go to File…"), Some("win.quick-open"));
        let this_file = gio::Menu::new();
        this_file.append(Some("Copy Path"), Some("win.copy-path"));
        this_file.append(Some("Show in Files"), Some("win.show-in-files"));
        this_file.append(Some("Open With…"), Some("win.open-with"));
        file_menu.append_section(None, &this_file);
        let options = gio::Menu::new();
        options.append(Some("Check Spelling"), Some("app.spell"));
        options.append(Some("Show Hidden Files"), Some("app.show-hidden"));
        file_menu.append_section(None, &options);
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("File")
            .menu_model(&file_menu)
            .build();
        header.pack_start(&menu_btn);
        let new = gtk::Button::from_icon_name("document-new-symbolic");
        new.set_tooltip_text(Some("New (Ctrl+N)"));
        new.set_action_name(Some("app.new"));
        header.pack_start(&new);
        let theme_btn = gtk::Button::from_icon_name("weather-clear-night-symbolic");
        theme_btn.set_tooltip_text(Some("Toggle dark mode (Ctrl+D)"));
        theme_btn.set_action_name(Some("app.toggle-theme"));
        header.pack_end(&theme_btn);
        let side_btn = gtk::ToggleButton::new();
        side_btn.set_icon_name("sidebar-show-symbolic");
        side_btn.set_tooltip_text(Some("Sidebar (F9)"));
        header.pack_start(&side_btn);
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
        let outline_list = gtk::ListBox::new();
        outline_list.set_activate_on_single_click(true);
        outline_list.set_placeholder(Some(&gtk::Label::builder().label("No headings").css_classes(["dim"]).margin_top(12).build()));
        let outline_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&outline_list)
            .build();
        outline_scroll.add_css_class("outline");
        let side_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(100)
            .vexpand(true)
            .build();
        side_stack.add_titled(&outline_scroll, Some("outline"), "Outline");
        let switcher = gtk::StackSwitcher::builder().stack(&side_stack).halign(gtk::Align::Center).build();
        switcher.add_css_class("side-switcher");
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.add_css_class("sidebar");
        side.set_width_request(260);
        // Its rows expand; the sidebar itself must not take the window's width.
        side.set_hexpand(false);
        side.append(&switcher);
        side.append(&side_stack);
        let outline_rev = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .transition_duration(150)
            .child(&side)
            .build();
        side_btn.bind_property("active", &outline_rev, "reveal-child").sync_create().build();
        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        hbox.append(&outline_rev);
        hbox.append(&scroller);
        scroller.set_hexpand(true);
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.append(&search);
        vbox.append(&hbox);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&vbox));
        win.set_titlebar(Some(&header));
        win.set_child(Some(&overlay));
        view.set_theme(theme());
        view.set_spell(state().spell.get());

        let w = Rc::new_cyclic(|me: &Weak<Win>| Win {
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
            closing: Cell::new(false),
            side_btn,
            side_stack,
            outline_list,
            files: FileTree::new(
                clone!(
                    #[strong]
                    me,
                    move |p| if let Some(w) = me.upgrade() {
                        w.open(p, true);
                    }
                ),
                clone!(
                    #[strong]
                    me,
                    move |root| if let Some(w) = me.upgrade() {
                        *w.root.borrow_mut() = Some(root);
                    }
                ),
            ),
            quick: QuickOpen::new(
                clone!(
                    #[strong]
                    me,
                    move |p| if let Some(w) = me.upgrade() {
                        w.open(p, true);
                    }
                ),
                clone!(
                    #[strong]
                    me,
                    move || if let Some(w) = me.upgrade() {
                        w.view.grab_focus();
                    }
                ),
            ),
            root: RefCell::new(None),
            outline: RefCell::new(Vec::new()),
        });
        w.side_stack.add_titled(&w.files.widget, Some("files"), "Files");
        w.files.set_show_hidden(state().hidden.get());
        overlay.add_overlay(&w.quick.widget);
        state().windows.borrow_mut().push(Rc::downgrade(&w));
        w.setup_actions();
        w.setup_search();
        w.setup_drop();
        w.setup_links();
        w.setup_editing();
        w.setup_outline();
        // The window owns the Rc through this handler and drops it on close.
        let keep = RefCell::new(Some(w.clone()));
        w.win.connect_destroy(move |_| {
            keep.borrow_mut().take();
        });
        match path {
            Some(p) => w.open(p, false),
            None => {
                w.view.set_empty();
                w.update_titles();
                // Untitled: browse the working directory, unless that is
                // just the home folder (launched from the desktop).
                let cwd = std::env::current_dir().ok().filter(|d| Some(d.as_os_str()) != std::env::var_os("HOME").as_deref() && d.parent().is_some());
                *w.root.borrow_mut() = cwd.map(|d| files::project_root(&d));
            }
        }
        let page = state().side_page.borrow().clone();
        w.side_stack.set_visible_child_name(&page);
        w.side_btn.set_active(state().sidebar.get());
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
    /// [FOLIO_SIDEBAR=outline|files] [FOLIO_QUICK=query] [FOLIO_DELAY=ms]
    /// renders the live window with GTK's renderer, saves it
    /// and exits. FOLIO_DELAY leaves time for scripted input.
    fn debug_screenshot(self: &Rc<Self>, out: String) {
        let w = self.clone();
        let delay = std::env::var("FOLIO_DELAY").ok().and_then(|d| d.parse().ok()).unwrap_or(600);
        glib::timeout_add_local_once(Duration::from_millis(delay), move || {
            if let Ok(q) = std::env::var("FOLIO_FIND") {
                w.search.set_search_mode(true);
                w.entry.set_text(&q);
            }
            if let Some(y) = std::env::var("FOLIO_SCROLL").ok().and_then(|y| y.parse().ok()) {
                w.view.scroll_to(y);
            }
            if let Ok(page) = std::env::var("FOLIO_SIDEBAR") {
                w.side_stack.set_visible_child_name(&page);
                w.side_btn.set_active(true);
            }
            if let Ok(q) = std::env::var("FOLIO_QUICK") {
                w.quick.show(w.root.borrow().clone(), state().hidden.get());
                w.quick.entry.set_text(&q);
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
        self.action("save", |w| w.save(|_| {}));
        self.action("save-as", |w| w.save_as(|_| {}));
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
        self.action("copy-path", |w| {
            if let Some(p) = w.path.borrow().as_ref() {
                w.win.clipboard().set_text(&p.to_string_lossy());
            }
        });
        self.action("show-in-files", |w| w.launch(false));
        self.action("open-with", |w| w.launch(true));
        self.action("quick-open", |w| {
            if w.quick.is_open() {
                w.quick.hide();
            } else {
                w.quick.show(w.root.borrow().clone(), state().hidden.get());
            }
        });
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
        drop.connect_drop(move |_, value, x, y| {
            let Some(w) = weak.upgrade() else { return false };
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let files = list.files();
            // Markdown opens; other files dropped on a document are linked there.
            let md = files.first().and_then(|f| f.path()).filter(|p| is_markdown(p));
            match md {
                Some(p) => w.open(p, true),
                None => {
                    let pt = gtk::graphene::Point::new(x as f32, y as f32);
                    let Some(at) = w.win.compute_point(&w.view, &pt) else { return false };
                    w.view.drop_files(&files, at.x() as f64, at.y() as f64);
                }
            }
            true
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
        if is_markdown(&path) && path.is_file() {
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
        // Never replace unsaved work: open elsewhere instead.
        if self.view.is_dirty() && self.path.borrow().as_ref() != Some(&path) {
            if let Some(app) = self.win.application() {
                Win::new(&app, Some(path));
            }
            return;
        }
        let (src, doc) = match load(&path) {
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
        self.view.set_doc(src, doc, base.clone(), false);
        self.set_root_for(&path);
        self.view.set_doc_dir(Some(base));
        self.refresh_outline();
        self.refresh_files();
        self.update_titles();
        self.watch(&path);
        self.refresh_search();
    }

    fn reload(self: &Rc<Self>) {
        let Some(path) = self.path.borrow().clone() else { return };
        // Mid-save files can briefly be missing or empty; keep the old view then.
        let Ok((src, doc)) = read_doc(&path) else { return };
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.view.set_doc(src, doc, base, true);
        self.update_titles();
        self.refresh_outline();
        self.refresh_search();
    }

    /// The file changed on disk: reload, unless it is our own save or
    /// there are unsaved edits (then ask).
    fn on_disk_change(self: &Rc<Self>) {
        let Some(path) = self.path.borrow().clone() else { return };
        let Ok(bytes) = std::fs::read(&path) else { return };
        if self.view.source().is_some_and(|s| s.as_bytes() == bytes) {
            return;
        }
        if !self.view.is_dirty() {
            return self.reload();
        }
        let dialog = gtk::AlertDialog::builder()
            .message("File changed on disk")
            .detail("It was changed by another program. Reload it and lose your unsaved changes?")
            .buttons(["Keep Mine", "Reload"])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.choose(Some(&self.win), gio::Cancellable::NONE, move |r| {
            if let (Some(w), Ok(1)) = (weak.upgrade(), r) {
                w.reload();
            }
        });
    }

    /// Keep showing the current folder while `path` is inside it; otherwise
    /// switch to the file's project (git work tree) or folder.
    fn set_root_for(&self, path: &Path) {
        let inside = self.root.borrow().as_ref().is_some_and(|r| path.starts_with(r));
        if !inside && let Some(dir) = path.parent() {
            *self.root.borrow_mut() = Some(files::project_root(dir));
        }
    }

    /// Update the files sidebar (only while it is shown).
    fn refresh_files(&self) {
        if !self.side_btn.is_active() || self.side_stack.visible_child_name().as_deref() != Some("files") {
            return;
        }
        if let Some(root) = self.root.borrow().as_ref() {
            self.files.set_root(root);
        }
        self.files.set_current(self.path.borrow().as_deref());
    }

    fn setup_outline(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.side_btn.connect_active_notify(move |b| {
            if state().sidebar.replace(b.is_active()) != b.is_active() {
                save_settings();
            }
            if let Some(w) = weak.upgrade() {
                w.refresh_outline();
                w.refresh_files();
            }
        });
        let weak = Rc::downgrade(self);
        self.side_stack.connect_visible_child_name_notify(move |s| {
            if let Some(name) = s.visible_child_name() {
                let changed = *state().side_page.borrow() != name;
                if changed {
                    *state().side_page.borrow_mut() = name.to_string();
                    save_settings();
                }
            }
            if let Some(w) = weak.upgrade() {
                w.refresh_outline();
                w.refresh_files();
            }
        });
        self.action("sidebar", |w| w.side_btn.set_active(!w.side_btn.is_active()));
        let weak = Rc::downgrade(self);
        self.outline_list.connect_row_activated(move |_, row| {
            let Some(w) = weak.upgrade() else { return };
            let off = w.outline.borrow().get(row.index() as usize).map(|h| h.2);
            if let Some(off) = off {
                w.view.scroll_to_off(off);
            }
        });
        let weak = Rc::downgrade(self);
        self.view.connect_scrolled(move || {
            if let Some(w) = weak.upgrade() {
                w.highlight_outline();
            }
        });
    }

    /// Rebuild the outline rows if the headings changed (only while shown).
    fn refresh_outline(&self) {
        if !self.side_btn.is_active() {
            return;
        }
        let new: Vec<(u8, String, usize)> = self.view.outline().into_iter().map(|h| (h.level, h.text, h.off)).collect();
        let same = {
            let old = self.outline.borrow();
            old.len() == new.len() && old.iter().zip(&new).all(|(a, b)| a.0 == b.0 && a.1 == b.1)
        };
        if !same {
            while let Some(row) = self.outline_list.row_at_index(0) {
                self.outline_list.remove(&row);
            }
            let top = new.iter().map(|h| h.0).min().unwrap_or(1);
            for (level, text, _) in &new {
                let depth = (level - top) as i32;
                let label = gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .tooltip_text(text)
                    .margin_start(14 * depth)
                    .build();
                if depth == 0 {
                    label.add_css_class("top");
                } else if depth >= 2 {
                    label.add_css_class("dim");
                }
                self.outline_list.append(&label);
            }
        }
        *self.outline.borrow_mut() = new;
        self.highlight_outline();
    }

    /// Select the row of the section at the top of the view.
    fn highlight_outline(&self) {
        if !self.side_btn.is_active() {
            return;
        }
        let row = self.view.current_heading().and_then(|i| self.outline_list.row_at_index(i as i32));
        match &row {
            Some(r) if !r.is_selected() => {
                self.outline_list.select_row(Some(r));
                // Keep it visible in the list without taking focus.
                if let (Some(b), Some(adj)) = (r.compute_bounds(&self.outline_list), self.outline_list.adjustment()) {
                    let (y, h) = (b.y() as f64, b.height() as f64);
                    if y < adj.value() || y + h > adj.value() + adj.page_size() {
                        adj.set_value(y - adj.page_size() / 3.0);
                    }
                }
            }
            None => self.outline_list.unselect_all(),
            _ => {}
        }
    }

    fn setup_editing(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.view.connect_change(move || {
            if let Some(w) = weak.upgrade() {
                w.update_titles();
                w.refresh_search();
                w.refresh_outline();
            }
        });
        let weak = Rc::downgrade(self);
        self.win.connect_close_request(move |win| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if w.closing.get() || !w.view.is_dirty() {
                return glib::Propagation::Proceed;
            }
            let name = w.display_name();
            let dialog = gtk::AlertDialog::builder()
                .message(format!("Save changes to “{name}”?"))
                .detail("Your changes will be lost if you don't save them.")
                .buttons(["Cancel", "Discard", "Save"])
                .cancel_button(0)
                .default_button(2)
                .modal(true)
                .build();
            let weak = Rc::downgrade(&w);
            dialog.choose(Some(win), gio::Cancellable::NONE, move |r| {
                let Some(w) = weak.upgrade() else { return };
                match r {
                    Ok(1) => {
                        w.closing.set(true);
                        w.win.close();
                    }
                    Ok(2) => w.save(|w| {
                        w.closing.set(true);
                        w.win.close();
                    }),
                    _ => {}
                }
            });
            glib::Propagation::Stop
        });
    }

    /// Save to the current file (or ask for one); `then` runs on success.
    fn save(self: &Rc<Self>, then: impl Fn(&Rc<Win>) + 'static) {
        let Some(path) = self.path.borrow().clone() else { return self.save_as(then) };
        if self.write(&path) {
            then(self);
        }
    }

    fn save_as(self: &Rc<Self>, then: impl Fn(&Rc<Win>) + 'static) {
        let dialog = gtk::FileDialog::builder().title("Save Markdown").modal(true).build();
        match self.path.borrow().as_ref() {
            Some(p) => dialog.set_initial_file(Some(&gio::File::for_path(p))),
            None => dialog.set_initial_name(Some("Untitled.md")),
        }
        let weak = Rc::downgrade(self);
        dialog.save(Some(&self.win), gio::Cancellable::NONE, move |res| {
            let (Some(w), Ok(file)) = (weak.upgrade(), res) else { return };
            let Some(path) = file.path() else { return };
            if w.write(&path) {
                let path = path.canonicalize().unwrap_or(path);
                let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
                w.view.set_base_dir(dir.clone());
                w.view.set_doc_dir(Some(dir));
                *w.path.borrow_mut() = Some(path.clone());
                w.set_root_for(&path);
                w.refresh_files();
                w.watch(&path);
                w.update_titles();
                then(&w);
            }
        });
    }

    /// Write the document atomically (temp file + rename, keeping permissions).
    fn write(&self, path: &Path) -> bool {
        let Some(src) = self.view.source() else { return false };
        let file = gio::File::for_path(path);
        match file.replace_contents(src.as_bytes(), None, false, gio::FileCreateFlags::NONE, gio::Cancellable::NONE) {
            Ok(_) => {
                self.view.mark_saved();
                true
            }
            Err(e) => {
                gtk::AlertDialog::builder()
                    .message("Could not save file")
                    .detail(format!("{}\n{e}", path.display()))
                    .build()
                    .show(Some(&self.win));
                false
            }
        }
    }

    /// Shows the file in the file manager, or opens it in an app the user picks.
    fn launch(self: &Rc<Self>, choose_app: bool) {
        let Some(path) = self.path.borrow().clone() else { return };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&path)));
        let win = self.win.clone();
        let done = move |r: Result<(), glib::Error>| {
            if let Err(e) = r
                && !e.matches(gtk::DialogError::Dismissed)
            {
                gtk::AlertDialog::builder().message("Could not open the file").detail(e.message()).build().show(Some(&win));
            }
        };
        if choose_app {
            launcher.set_always_ask(true);
            launcher.launch(Some(&self.win), gio::Cancellable::NONE, done);
        } else {
            launcher.open_containing_folder(Some(&self.win), gio::Cancellable::NONE, done);
        }
    }

    fn display_name(&self) -> String {
        match self.path.borrow().as_ref() {
            Some(p) => p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into()),
            None => "Untitled".into(),
        }
    }

    fn refresh_search(&self) {
        if self.search.is_search_mode() && !self.entry.text().is_empty() {
            let n = self.view.find(&self.entry.text());
            self.count.set_text(&if n == 0 { "No matches".into() } else { format!("{n} matches") });
        }
    }

    fn update_titles(&self) {
        let name = self.display_name();
        let dirty = if self.view.is_dirty() { "• " } else { "" };
        self.title.set_text(&format!("{dirty}{name}"));
        let path = self.path.borrow();
        let dir = path.as_ref().and_then(|p| p.parent()).map(|d| d.display().to_string()).unwrap_or_default();
        let dir = match std::env::var("HOME") {
            Ok(home) if dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
            _ => dir,
        };
        self.subtitle.set_text(&dir);
        self.subtitle.set_visible(!dir.is_empty());
        self.win.set_title(Some(&format!("{dirty}{name} — Folio")));
        for a in ["copy-path", "show-in-files", "open-with"] {
            if let Some(a) = self.win.lookup_action(a).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(path.is_some());
            }
        }
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
                        w.on_disk_change();
                    }
                });
                *w.reload_timer.borrow_mut() = Some(id);
            });
        }
        *self.monitor.borrow_mut() = monitor;
    }
}
