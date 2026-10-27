//! The files sidebar (a lazily loaded tree of the document's folder) and
//! quick open (`Ctrl+P`: fuzzy search over the Markdown files below it).

use gtk::glib::{self, clone};
use gtk::{gdk, gio, prelude::*};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

pub fn is_markdown(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd" | "mdx" | "txt"))
}

/// Folders go to file never searches: repository internals and
/// build/dependency output (the tree still shows them).
fn skip_dir(name: &str) -> bool {
    matches!(name, ".git" | "node_modules" | "target" | "__pycache__")
}

/// The folder to browse for a document in `dir`: its git work tree if it is
/// in one, else the folder itself.
pub fn project_root(dir: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for d in dir.ancestors() {
        if Some(d) == home.as_deref() {
            break;
        }
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
    }
    dir.to_path_buf()
}

/// Markdown files below `root` (relative paths), breadth first, giving up
/// after `limit` directory entries so a huge folder can't stall the search.
/// Hidden files and folders are included only with `hidden`.
pub fn walk(root: &Path, limit: usize, hidden: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([PathBuf::new()]);
    let mut seen = 0;
    while let Some(rel) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            seen += 1;
            if seen > limit {
                return out;
            }
            let name = e.file_name();
            let Some(n) = name.to_str() else { continue };
            let Ok(ft) = e.file_type() else { continue };
            let path = rel.join(n);
            if !hidden && n.starts_with('.') {
                continue;
            }
            if ft.is_dir() {
                if !skip_dir(n) {
                    queue.push_back(path);
                }
            } else if is_markdown(&path) {
                out.push(path);
            }
        }
    }
    out
}

/// Fuzzy match: every query character in order, case-insensitively. Higher
/// is better: matches in the file name, at word starts and in runs score
/// more; long paths score less.
pub fn score(query: &str, path: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(-(path.len() as i32));
    }
    let p: Vec<char> = path.chars().flat_map(char::to_lowercase).collect();
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()).collect();
    let name_start = p.iter().rposition(|&c| c == '/').map_or(0, |i| i + 1);
    // The extension earns nothing: "roadm" means ROADMAP.md, not road.md.
    let ext = p[name_start..].iter().rposition(|&c| c == '.').filter(|&i| i > 0).map_or(p.len(), |i| name_start + i);
    let run = |from: usize| -> Option<i32> {
        let mut s = 0;
        let mut at = from;
        let mut prev: Option<usize> = None;
        for &qc in &q {
            let k = (at..p.len()).find(|&k| p[k] == qc)?;
            s += 1;
            if k < ext {
                if prev == Some(k.wrapping_sub(1)) {
                    s += 6;
                }
                if k == 0 || matches!(p[k - 1], '/' | '-' | '_' | ' ' | '.') {
                    s += 8;
                }
                if k >= name_start {
                    s += 4;
                }
            }
            prev = Some(k);
            at = k + 1;
        }
        Some(s)
    };
    // The whole query in the file name beats a match spread over folders.
    let best = match run(name_start) {
        Some(s) => s + 10,
        None => run(0)?,
    };
    Some(best * 16 - p.len() as i32)
}

/// Indices of the best `n` matches, best first.
pub fn rank(files: &[String], query: &str, n: usize) -> Vec<usize> {
    let mut v: Vec<(i32, usize)> = files.iter().enumerate().filter_map(|(i, f)| score(query, f).map(|s| (s, i))).collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then(files[a.1].cmp(&files[b.1])));
    v.into_iter().take(n).map(|(_, i)| i).collect()
}

type OpenHandler = Rc<dyn Fn(PathBuf)>;

const ATTRS: &str = "standard::name,standard::display-name,standard::type,standard::is-hidden";

fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast()
}

/// What the tree shows, shared by the filters of all its folders.
#[derive(Default)]
struct Shown {
    hidden: std::cell::Cell<bool>,
    /// The open file: it and the folders above it show even when hidden.
    current: RefCell<Option<PathBuf>>,
    filters: RefCell<Vec<glib::WeakRef<gtk::CustomFilter>>>,
}

impl Shown {
    fn changed(&self) {
        self.filters.borrow_mut().retain(|f| match f.upgrade() {
            Some(f) => {
                f.changed(gtk::FilterChange::Different);
                true
            }
            None => false,
        });
    }
}

/// Folders first, then Markdown files; hidden ones only when asked for, or
/// on the way to the open file.
fn dir_model(dir: &gio::File, shown: &Rc<Shown>) -> gio::ListModel {
    let list = gtk::DirectoryList::new(Some(ATTRS), Some(dir));
    let sh = Rc::downgrade(shown);
    let filter = gtk::CustomFilter::new(move |o| {
        let (Some(info), Some(sh)) = (o.downcast_ref::<gio::FileInfo>(), sh.upgrade()) else { return false };
        let dir = info.file_type() == gio::FileType::Directory;
        if !dir && !is_markdown(&info.name()) {
            return false;
        }
        let hidden = info.is_hidden() || info.name().to_str().is_some_and(|n| n.starts_with('.'));
        sh.hidden.get()
            || !hidden
            || sh.current.borrow().as_ref().is_some_and(|c| info_file(info).and_then(|f| f.path()).is_some_and(|p| c.starts_with(p)))
    });
    shown.filters.borrow_mut().push(filter.downgrade());
    let sorter = gtk::CustomSorter::new(|a, b| {
        let (Some(a), Some(b)) = (a.downcast_ref::<gio::FileInfo>(), b.downcast_ref::<gio::FileInfo>()) else {
            return gtk::Ordering::Equal;
        };
        let dir = |i: &gio::FileInfo| i.file_type() != gio::FileType::Directory;
        let key = |i: &gio::FileInfo| i.display_name().to_lowercase();
        (dir(a), key(a)).cmp(&(dir(b), key(b))).into()
    });
    let filtered = gtk::FilterListModel::new(Some(list), Some(filter));
    gtk::SortListModel::new(Some(filtered), Some(sorter)).upcast()
}

/// The files sidebar: a tree of one folder with its sub-folders and
/// Markdown files. Folders are read when expanded and watched for changes.
pub struct FileTree {
    pub widget: gtk::Box,
    title: gtk::Label,
    view: gtk::ListView,
    root: RefCell<Option<PathBuf>>,
    shown: Rc<Shown>,
    /// Rows on screen and their paths, to mark the open file.
    rows: RefCell<Vec<(glib::WeakRef<gtk::Widget>, PathBuf)>>,
    on_open: OpenHandler,
    on_root: Rc<dyn Fn(PathBuf)>,
    reveal_timer: RefCell<Option<glib::SourceId>>,
}

impl FileTree {
    /// `on_open` gets a Markdown file the user clicked; `on_root` the new
    /// root after going up a folder.
    pub fn new(on_open: impl Fn(PathBuf) + 'static, on_root: impl Fn(PathBuf) + 'static) -> Rc<FileTree> {
        // Ellipsized labels still ask for their whole text's width;
        // `max_width_chars` stops long names from widening the sidebar.
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .max_width_chars(1)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .build();
        title.add_css_class("files-root");
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some("Parent folder"));
        up.add_css_class("flat");
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        head.add_css_class("files-head");
        head.append(&title);
        head.append(&up);
        let view = gtk::ListView::new(None::<gtk::NoSelection>, None::<gtk::ListItemFactory>);
        view.add_css_class("files");
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        widget.append(&head);
        widget.append(&scroll);
        let t = Rc::new(FileTree {
            widget,
            title,
            view,
            root: RefCell::new(None),
            shown: Rc::default(),
            rows: RefCell::new(Vec::new()),
            on_open: Rc::new(on_open),
            on_root: Rc::new(on_root),
            reveal_timer: RefCell::new(None),
        });
        t.view.set_factory(Some(&t.factory()));
        let weak = Rc::downgrade(&t);
        up.connect_clicked(move |_| {
            let Some(t) = weak.upgrade() else { return };
            let parent = t.root.borrow().as_ref().and_then(|r| r.parent()).map(Path::to_path_buf);
            if let Some(p) = parent {
                t.set_root(&p);
                (t.on_root)(p);
            }
        });
        let weak = Rc::downgrade(&t);
        t.view.connect_activate(move |v, pos| {
            if let (Some(t), Some(row)) = (weak.upgrade(), v.model().and_then(|m| m.item(pos)).and_downcast::<gtk::TreeListRow>()) {
                t.activate(&row);
            }
        });
        t
    }

    fn factory(self: &Rc<Self>) -> gtk::SignalListItemFactory {
        let f = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        f.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let icon = gtk::Image::new();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .max_width_chars(1)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.set_hexpand(true);
            row.append(&icon);
            row.append(&label);
            let exp = gtk::TreeExpander::new();
            exp.set_child(Some(&row));
            item.set_child(Some(&exp));
            // One click opens a file or expands a folder.
            let click = gtk::GestureClick::new();
            click.connect_released(clone!(
                #[weak]
                item,
                #[strong]
                weak,
                move |g, _, _, _| {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let (Some(t), Some(r)) = (weak.upgrade(), item.item().and_downcast::<gtk::TreeListRow>()) {
                        t.activate(&r);
                    }
                }
            ));
            row.add_controller(click);
        });
        let weak = Rc::downgrade(self);
        f.connect_bind(move |_, item| {
            let Some(t) = weak.upgrade() else { return };
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(exp), Some(row)) = (item.child().and_downcast::<gtk::TreeExpander>(), item.item().and_downcast::<gtk::TreeListRow>()) else {
                return;
            };
            exp.set_list_row(Some(&row));
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            let Some(bx) = exp.child().and_downcast::<gtk::Box>() else { return };
            let dir = info.file_type() == gio::FileType::Directory;
            if let Some(icon) = bx.first_child().and_downcast::<gtk::Image>() {
                icon.set_icon_name(Some(if dir { "folder-symbolic" } else { "text-x-generic-symbolic" }));
            }
            if let Some(label) = bx.last_child().and_downcast::<gtk::Label>() {
                label.set_text(&info.display_name());
                label.set_tooltip_text(Some(&info.display_name()));
            }
            // The expander fills the row, so the highlight spans the indentation.
            let row_w: gtk::Widget = exp.clone().upcast();
            if let Some(path) = info_file(&info).and_then(|f| f.path()) {
                let current = t.shown.current.borrow().as_ref() == Some(&path);
                set_class(&row_w, "current", current);
                t.rows.borrow_mut().push((row_w.downgrade(), path));
            } else {
                set_class(&row_w, "current", false);
            }
        });
        let weak = Rc::downgrade(self);
        f.connect_unbind(move |_, item| {
            let Some(t) = weak.upgrade() else { return };
            let row_w = item.downcast_ref::<gtk::ListItem>().and_then(|i| i.child());
            t.rows.borrow_mut().retain(|(w, _)| w.upgrade().is_some_and(|b| Some(&b) != row_w.as_ref()));
        });
        f
    }

    fn activate(&self, row: &gtk::TreeListRow) {
        let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
        if info.file_type() == gio::FileType::Directory {
            row.set_expanded(!row.is_expanded());
        } else if let Some(p) = info_file(&info).and_then(|f| f.path()) {
            (self.on_open)(p);
        }
    }

    /// Show `root` (no-op if it is already shown).
    pub fn set_root(&self, root: &Path) {
        if self.root.borrow().as_deref() == Some(root) {
            return;
        }
        *self.root.borrow_mut() = Some(root.to_path_buf());
        let name = match std::env::var("HOME") {
            Ok(h) if root.starts_with(&h) => format!("~{}", &root.to_string_lossy()[h.len()..]),
            _ => root.display().to_string(),
        };
        self.title.set_text(&name);
        self.title.set_tooltip_text(Some(&root.display().to_string()));
        self.rows.borrow_mut().clear();
        self.shown.filters.borrow_mut().clear();
        let shown = Rc::downgrade(&self.shown);
        let tree = gtk::TreeListModel::new(dir_model(&gio::File::for_path(root), &self.shown), false, false, move |o| {
            let info = o.downcast_ref::<gio::FileInfo>()?;
            let dir = info_file(info).filter(|_| info.file_type() == gio::FileType::Directory)?;
            Some(dir_model(&dir, &shown.upgrade()?))
        });
        self.view.set_model(Some(&gtk::NoSelection::new(Some(tree))));
    }

    /// Mark the open document's row, expanding the folders above it and
    /// scrolling to it.
    pub fn set_current(self: &Rc<Self>, path: Option<&Path>) {
        let new = path.map(Path::to_path_buf);
        if *self.shown.current.borrow() != new {
            *self.shown.current.borrow_mut() = new;
            // A hidden folder on the way to the file may now show.
            self.shown.changed();
        }
        if let Some(p) = path {
            self.reveal(p.to_path_buf());
        }
        self.rows.borrow_mut().retain(|(w, p)| match w.upgrade() {
            Some(b) => {
                set_class(&b, "current", Some(p.as_path()) == path);
                true
            }
            None => false,
        });
    }
}

impl FileTree {
    /// Folders load asynchronously, so expand one level per tick until the
    /// file's row exists (giving up after two seconds).
    fn reveal(self: &Rc<Self>, target: PathBuf) {
        if let Some(id) = self.reveal_timer.take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let mut tries = 0;
        let id = glib::timeout_add_local(Duration::from_millis(30), move || {
            let Some(t) = weak.upgrade() else { return glib::ControlFlow::Break };
            tries += 1;
            if t.reveal_step(&target) || tries > 60 {
                t.reveal_timer.take();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
        *self.reveal_timer.borrow_mut() = Some(id);
    }

    /// Returns true when done: the row is shown, or can't be.
    fn reveal_step(&self, target: &Path) -> bool {
        let Some(model) = self.view.model() else { return true };
        if !self.root.borrow().as_ref().is_some_and(|r| target.starts_with(r)) {
            return true;
        }
        for i in 0..model.n_items() {
            let Some(row) = model.item(i).and_downcast::<gtk::TreeListRow>() else { continue };
            let Some(p) = row.item().and_downcast::<gio::FileInfo>().and_then(|f| info_file(&f)).and_then(|f| f.path()) else {
                continue;
            };
            if p == target {
                self.view.scroll_to(i, gtk::ListScrollFlags::NONE, None);
                return true;
            }
            if target.starts_with(&p) && !row.is_expanded() {
                row.set_expanded(true);
                return false;
            }
        }
        false
    }
}

impl FileTree {
    pub fn set_show_hidden(&self, on: bool) {
        if self.shown.hidden.replace(on) != on {
            self.shown.changed();
        }
    }
}

fn set_class(w: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        w.add_css_class(class);
    } else {
        w.remove_css_class(class);
    }
}

/// Quick open: a search field over the window listing the Markdown files
/// below a folder, best fuzzy match first.
pub struct QuickOpen {
    pub widget: gtk::Box,
    pub entry: gtk::SearchEntry,
    list: gtk::ListBox,
    root: RefCell<Option<PathBuf>>,
    files: RefCell<Vec<String>>,
    /// The files shown, in list order.
    shown: RefCell<Vec<usize>>,
    on_open: OpenHandler,
    on_close: Rc<dyn Fn()>,
}

/// Directory entries visited at most per search.
const WALK_LIMIT: usize = 50_000;

impl QuickOpen {
    pub fn new(on_open: impl Fn(PathBuf) + 'static, on_close: impl Fn() + 'static) -> Rc<QuickOpen> {
        let entry = gtk::SearchEntry::builder().placeholder_text("Go to file").hexpand(true).build();
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Browse);
        list.set_activate_on_single_click(true);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(380)
            .child(&list)
            .build();
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Start)
            .width_request(540)
            .margin_top(10)
            .visible(false)
            .build();
        widget.add_css_class("quick-open");
        widget.append(&entry);
        widget.append(&scroll);
        let q = Rc::new(QuickOpen {
            widget,
            entry,
            list,
            root: RefCell::new(None),
            files: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            on_open: Rc::new(on_open),
            on_close: Rc::new(on_close),
        });
        q.setup();
        q
    }

    fn setup(self: &Rc<Self>) {
        // A widget can only take focus once it is on screen.
        let entry = self.entry.clone();
        self.widget.connect_map(move |_| {
            entry.grab_focus();
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_search_changed(move |_| {
            if let Some(q) = weak.upgrade() {
                q.refilter();
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_activate(move |_| {
            if let Some(q) = weak.upgrade() {
                let i = q.list.selected_row().map_or(0, |r| r.index());
                q.open_index(i);
            }
        });
        let weak = Rc::downgrade(self);
        self.entry.connect_stop_search(move |_| {
            if let Some(q) = weak.upgrade() {
                q.hide();
            }
        });
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let Some(q) = weak.upgrade() {
                q.open_index(row.index());
            }
        });
        // Up/Down move through the results while typing.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(q) = weak.upgrade() else { return glib::Propagation::Proceed };
            let step = match key {
                gdk::Key::Down | gdk::Key::KP_Down => 1,
                gdk::Key::Up | gdk::Key::KP_Up => -1,
                gdk::Key::Page_Down => 8,
                gdk::Key::Page_Up => -8,
                _ => return glib::Propagation::Proceed,
            };
            let n = q.shown.borrow().len() as i32;
            if n > 0 {
                let cur = q.list.selected_row().map_or(0, |r| r.index());
                if let Some(r) = q.list.row_at_index((cur + step).clamp(0, n - 1)) {
                    q.list.select_row(Some(&r));
                    // Keep it in view.
                    if let (Some(b), Some(adj)) = (r.compute_bounds(&q.list), q.list.adjustment()) {
                        let (y, h) = (b.y() as f64, b.height() as f64);
                        if y < adj.value() {
                            adj.set_value(y);
                        } else if y + h > adj.value() + adj.page_size() {
                            adj.set_value(y + h - adj.page_size());
                        }
                    }
                }
            }
            glib::Propagation::Stop
        });
        self.entry.add_controller(keys);
        // Clicking elsewhere closes it.
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(q) = weak.upgrade()
                && q.widget.is_visible()
            {
                q.hide();
            }
        });
        self.widget.add_controller(focus);
    }

    pub fn is_open(&self) -> bool {
        self.widget.is_visible()
    }

    /// Show it, listing the files below `root`.
    pub fn show(self: &Rc<Self>, root: Option<PathBuf>, hidden: bool) {
        self.entry.set_text("");
        // Focus moves to the entry once it is mapped (see `setup`).
        self.widget.set_visible(true);
        let same = *self.root.borrow() == root;
        if !same {
            self.files.borrow_mut().clear();
        }
        *self.root.borrow_mut() = root.clone();
        self.refilter();
        let Some(root) = root else { return };
        // Walk again each time so new files show up; the old list shows meanwhile.
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let r = root.clone();
            let Ok(files) = gio::spawn_blocking(move || walk(&r, WALK_LIMIT, hidden)).await else { return };
            let Some(q) = weak.upgrade() else { return };
            if q.root.borrow().as_ref() != Some(&root) {
                return;
            }
            *q.files.borrow_mut() = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
            q.refilter();
        });
    }

    pub fn hide(&self) {
        if self.widget.is_visible() {
            self.widget.set_visible(false);
            (self.on_close)();
        }
    }

    fn open_index(&self, i: i32) {
        let path = {
            let shown = self.shown.borrow();
            let files = self.files.borrow();
            let root = self.root.borrow();
            match (usize::try_from(i).ok().and_then(|i| shown.get(i)), root.as_ref()) {
                (Some(&k), Some(root)) => root.join(&files[k]),
                _ => return,
            }
        };
        self.hide();
        (self.on_open)(path);
    }

    fn refilter(&self) {
        while let Some(r) = self.list.row_at_index(0) {
            self.list.remove(&r);
        }
        let files = self.files.borrow();
        let shown = rank(&files, &self.entry.text(), 50);
        for &k in &shown {
            let f = &files[k];
            let (dir, name) = match f.rfind('/') {
                Some(s) => (&f[..s], &f[s + 1..]),
                None => ("", f.as_str()),
            };
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            let n = gtk::Label::builder().label(name).xalign(0.0).max_width_chars(40).ellipsize(gtk::pango::EllipsizeMode::End).build();
            n.add_css_class("name");
            let d = gtk::Label::builder()
                .label(dir)
                .xalign(0.0)
                .hexpand(true)
                .max_width_chars(1)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .build();
            d.add_css_class("dim");
            row.append(&n);
            row.append(&d);
            self.list.append(&row);
        }
        if shown.is_empty() {
            let msg = if self.root.borrow().is_none() { "Open or save a file first" } else { "No matching files" };
            let l = gtk::Label::builder().label(msg).css_classes(["dim"]).margin_top(8).margin_bottom(8).build();
            self.list.set_placeholder(Some(&l));
        }
        if let Some(r) = self.list.row_at_index(0) {
            self.list.select_row(Some(&r));
        }
        drop(files);
        *self.shown.borrow_mut() = shown;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_prefers_file_names_and_word_starts() {
        let files: Vec<String> = ["docs/HANDOFF.md", "README.md", "docs/ROADMAP.md", "notes/random/readme-old.md", "src/road.md"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let top = |q: &str| rank(&files, q, 5).into_iter().map(|i| files[i].as_str()).collect::<Vec<_>>();
        assert_eq!(top("readme")[0], "README.md");
        assert_eq!(top("roadm")[0], "docs/ROADMAP.md");
        assert_eq!(top("hand")[0], "docs/HANDOFF.md");
        assert_eq!(top("dhan")[0], "docs/HANDOFF.md");
        assert!(top("xyz").is_empty());
        assert_eq!(top("").len(), 5);
    }

    #[test]
    fn walk_finds_markdown_and_skips_hidden() {
        let dir = std::env::temp_dir().join(format!("folio-walk-{}", std::process::id()));
        for d in ["a/b", ".git", "node_modules/x", ".notes"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        for f in ["top.md", "a/b/deep.markdown", "a/code.rs", ".git/x.md", "node_modules/x/y.md", ".hidden.md", ".notes/n.md"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let sorted = |mut v: Vec<PathBuf>| {
            v.sort();
            v
        };
        let p = PathBuf::from;
        assert_eq!(sorted(walk(&dir, 1000, false)), [p("a/b/deep.markdown"), p("top.md")]);
        // Hidden ones on request; repository internals never.
        assert_eq!(sorted(walk(&dir, 1000, true)), [p(".hidden.md"), p(".notes/n.md"), p("a/b/deep.markdown"), p("top.md")]);
        assert_eq!(project_root(&dir.join("a/b")), dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
