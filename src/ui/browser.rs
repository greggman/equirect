use std::path::PathBuf;

pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mkv", "webm"];

// ── Location ──────────────────────────────────────────────────────────────────

/// A location in the browser: either a local filesystem path or a remote URL.
#[derive(Clone, Debug, PartialEq)]
pub enum Location {
    Local(PathBuf),
    /// An `http://` or `https://` URL (file or directory).
    Remote(String),
}

impl Location {
    /// Human-readable string shown in the header bar.
    pub fn display(&self) -> String {
        match self {
            Location::Local(p)  => p.display().to_string(),
            Location::Remote(u) => u.clone(),
        }
    }

    /// Parent location, or `None` if already at root.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Local(p) => p.parent().map(|p| Location::Local(p.to_path_buf())),
            Location::Remote(u) => {
                let parent = crate::net::parent_url(u);
                if parent.trim_end_matches('/') != u.trim_end_matches('/') {
                    Some(Location::Remote(parent))
                } else {
                    None
                }
            }
        }
    }

    pub fn as_local(&self) -> Option<&PathBuf> {
        if let Location::Local(p) = self { Some(p) } else { None }
    }

    #[allow(dead_code)]
    pub fn as_remote(&self) -> Option<&str> {
        if let Location::Remote(u) = self { Some(u) } else { None }
    }
}

// ── BrowserEntry ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub enum BrowserEntry {
    Parent,
    Dir(String, Location),
    Video(String, Location),
}

// ── BrowserState ─────────────────────────────────────────────────────────────

pub struct BrowserState {
    pub location:               Location,
    pub entries:                Vec<BrowserEntry>,
    pub current_video:          Option<Location>,
    /// Set to true after creation or navigation; causes the list to scroll to
    /// the current video on the next draw.
    pub needs_scroll_to_current: bool,
    /// Set to true when entries are freshly loaded (not served from cache).
    /// The app reads this to update the folder cache, then clears it.
    pub just_loaded: bool,
    /// Toggled by the "Volumes" button — shows the drive picker inline.
    pub show_volumes: bool,
    // Async loading state for remote locations.
    loading_rx: Option<std::sync::mpsc::Receiver<Result<Vec<BrowserEntry>, String>>>,
    pub is_loading:  bool,
    pub load_error:  Option<String>,
}

/// Actions the browser UI wants the app to perform.
#[derive(Default)]
pub struct BrowserActions {
    pub play:          Option<Location>,
    /// Navigate via ".." (go up) — destination may be served from cache.
    pub navigate:      Option<Location>,
    /// User explicitly clicked a folder in the list — invalidate its cache entry.
    pub select_dir:    Option<Location>,
    pub close:         bool,
    /// The root path of the volume the user selected in the volumes popup.
    /// The app resolves which directory to navigate to within that volume.
    pub select_volume: Option<PathBuf>,
}

impl BrowserState {
    /// `cached`: pre-loaded entries from the folder cache; if `Some`, skip the
    /// actual read.  Pass `None` to always read fresh.
    pub fn new(location: Location, current_video: Option<Location>, cached: Option<Vec<BrowserEntry>>) -> Self {
        let mut s = Self {
            location,
            entries: Vec::new(),
            current_video,
            needs_scroll_to_current: true,
            just_loaded: false,
            show_volumes: false,
            loading_rx: None,
            is_loading: false,
            load_error: None,
        };
        if let Some(entries) = cached {
            s.entries = entries;
        } else {
            s.start_load();
        }
        s
    }

    /// Navigate to `location`.  Pass cached entries to skip the real read, or
    /// `None` to always read fresh (e.g. after cache invalidation).
    pub fn navigate_to(&mut self, location: Location, cached: Option<Vec<BrowserEntry>>) {
        self.location                = location;
        self.entries                 = Vec::new();
        self.load_error              = None;
        self.needs_scroll_to_current = false;
        self.just_loaded             = false;
        if let Some(entries) = cached {
            self.entries = entries;
        } else {
            self.start_load();
        }
    }

    /// Call once per frame.  If an async remote load has completed, populate
    /// `entries`.
    pub fn poll_loading(&mut self) {
        let Some(rx) = &self.loading_rx else { return };
        if let Ok(result) = rx.try_recv() {
            self.loading_rx = None;
            self.is_loading = false;
            match result {
                Ok(entries) => {
                    self.entries     = entries;
                    self.load_error  = None;
                    self.just_loaded = true;
                }
                Err(e) => self.load_error = Some(e),
            }
        }
    }

    // ── private ───────────────────────────────────────────────────────────────

    fn start_load(&mut self) {
        match self.location.clone() {
            Location::Local(path) => {
                self.is_loading = false;
                self.loading_rx = None;
                self.refresh_local(&path);
                self.just_loaded = true;
            }
            Location::Remote(url) => {
                self.is_loading = true;
                let (tx, rx) = std::sync::mpsc::channel();
                self.loading_rx = Some(rx);
                std::thread::spawn(move || {
                    let result = crate::net::list_http_dir(&url).map(|items| {
                        let mut entries: Vec<BrowserEntry> = vec![BrowserEntry::Parent];
                        for item in items {
                            let loc = Location::Remote(item.url);
                            if item.is_dir {
                                entries.push(BrowserEntry::Dir(item.name, loc));
                            } else {
                                entries.push(BrowserEntry::Video(item.name, loc));
                            }
                        }
                        entries
                    }).map_err(|e| e.to_string());
                    let _ = tx.send(result);
                });
            }
        }
    }

    fn refresh_local(&mut self, path: &PathBuf) {
        self.entries = load_local_entries(path);
    }
}

/// Build a full directory listing for a local path in the same format the
/// browser uses (Parent first, then sorted dirs, then sorted videos).
/// Exposed so other code (e.g. prev/next navigation) can share it.
pub fn load_local_entries(path: &PathBuf) -> Vec<BrowserEntry> {
    let mut dirs:   Vec<(String, PathBuf)> = Vec::new();
    let mut videos: Vec<(String, PathBuf)> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            let p    = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') { continue; }
            if p.is_dir() {
                dirs.push((name, p));
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if VIDEO_EXTS.iter().any(|&v| v.eq_ignore_ascii_case(ext)) {
                    videos.push((name, p));
                }
            }
        }
    }

    dirs.sort_by(  |(a, _), (b, _)| a.cmp(b));
    videos.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut entries = vec![BrowserEntry::Parent];
    for (name, p) in dirs   { entries.push(BrowserEntry::Dir  (name, Location::Local(p))); }
    for (name, p) in videos { entries.push(BrowserEntry::Video(name, Location::Local(p))); }
    entries
}

/// Build entries from a remote HTTP listing in the same format the browser uses.
pub fn remote_entries(items: Vec<crate::net::RemoteItem>) -> Vec<BrowserEntry> {
    let mut entries = vec![BrowserEntry::Parent];
    for item in items {
        let loc = Location::Remote(item.url);
        if item.is_dir {
            entries.push(BrowserEntry::Dir(item.name, loc));
        } else {
            entries.push(BrowserEntry::Video(item.name, loc));
        }
    }
    entries
}


// ── draw ──────────────────────────────────────────────────────────────────────

use super::draw_list::Color;
use super::icons;
use super::ui::{Align, FONT_DEFAULT, ROW_HOVER, ROW_SELECTED, TEXT, TEXT_MUTED, Ui};

const ROW_H: f32 = 34.0;
const YELLOW:    Color = Color { r: 1.0, g: 0.92, b: 0.30, a: 1.0 };
const RED:       Color = Color { r: 1.0, g: 0.30, b: 0.30, a: 1.0 };
const LOAD_HINT: Color = Color { r: 0.59, g: 0.78, b: 1.0,  a: 1.0 };

pub fn draw(ui: &mut Ui, state: &mut BrowserState) -> BrowserActions {
    state.poll_loading();

    let mut actions = BrowserActions::default();

    let scroll_to_current  = state.needs_scroll_to_current;
    state.needs_scroll_to_current = false;
    let current_video      = state.current_video.clone();
    let parent_loc         = state.location.parent();
    let dir_display        = state.location.display();
    let is_local           = state.location.as_local().is_some();

    // ── header: path label, volumes toggle, close ────────────────────────────
    // 44 = tallest child (close button: 28 icon + 12 pad).  horizontal_h
    // centers the (shorter) path label vertically against the buttons.
    ui.horizontal_h(44.0, |ui| {
        ui.label_styled(&dir_display, FONT_DEFAULT, TEXT_MUTED);
        ui.horizontal_rtl(|ui| {
            if ui.icon_button(icons::ICON_CLOSE, 28.0, false) {
                actions.close = true;
            }
            if is_local {
                if ui.button("Volumes") {
                    state.show_volumes = !state.show_volumes;
                }
            }
        });
    });
    ui.separator();

    // ── volumes popup (inline — replaces the entry list while open) ──────────
    if state.show_volumes && is_local {
        let vols = crate::volumes::list_volumes();
        if vols.is_empty() {
            ui.label_styled("(no volumes found)", FONT_DEFAULT, TEXT_MUTED);
        } else {
            ui.scroll_area(0xC0_FFEEu64, ui.available_height(), |ui| {
                for vol in &vols {
                    let rect = ui.allocate_full_width(ROW_H);
                    let bg = if ui.hovered(rect) { ROW_HOVER } else { Color::TRANSPARENT };
                    if bg.a > 0.0 { ui.draw.rect(rect, bg); }
                    ui.draw_text_in(rect, &vol.label, FONT_DEFAULT, TEXT, Align::Left);
                    if ui.activated(rect) {
                        actions.select_volume = Some(vol.root.clone());
                        state.show_volumes    = false;
                    }
                }
            });
        }
        return actions;
    }

    // ── loading / error states ───────────────────────────────────────────────
    if state.is_loading {
        ui.label_styled("Loading…", FONT_DEFAULT, LOAD_HINT);
        return actions;
    }
    if let Some(err) = state.load_error.clone() {
        ui.label_styled(&format!("Error: {err}"), FONT_DEFAULT, RED);
        return actions;
    }

    // ── scrollable list ──────────────────────────────────────────────────────
    let viewport_h = ui.available_height();

    // If we just navigated, scroll the current video into view.  We do this by
    // pre-seeding the scroll_area's stored offset before the area renders.
    if scroll_to_current {
        if let Some(idx) = state.entries.iter().position(|e| {
            matches!(e, BrowserEntry::Video(_, loc) if current_video.as_ref() == Some(loc))
        }) {
            let off = (idx as f32 * (ROW_H + 2.0) - viewport_h / 2.0 + ROW_H / 2.0).max(0.0);
            ui.set_scroll_offset(BROWSER_LIST_ID, off);
        }
    }

    ui.scroll_area(BROWSER_LIST_ID, viewport_h, |ui| {
        for entry in &state.entries {
            let (display, is_selected, text_color, nav_up, select_dir, play_loc):
                (String, bool, Color, Option<Location>, Option<Location>, Option<Location>) = match entry
            {
                BrowserEntry::Parent => (
                    "..".to_string(),
                    false, TEXT,
                    parent_loc.clone(), None, None,
                ),
                BrowserEntry::Dir(name, loc) => (
                    format!("📁  {name}"),
                    false, TEXT,
                    None, Some(loc.clone()), None,
                ),
                BrowserEntry::Video(name, loc) => {
                    let is_cur = current_video.as_ref() == Some(loc);
                    let color  = if is_cur { YELLOW } else { TEXT };
                    (name.clone(), is_cur, color, None, None, Some(loc.clone()))
                }
            };

            let rect = ui.allocate_full_width(ROW_H);
            let bg = if ui.hovered(rect) {
                ROW_HOVER
            } else if is_selected {
                ROW_SELECTED
            } else {
                Color::TRANSPARENT
            };
            if bg.a > 0.0 { ui.draw.rect(rect, bg); }
            ui.draw_text_in(rect, &display, FONT_DEFAULT, text_color, Align::Left);

            if ui.activated(rect) {
                if let Some(loc) = nav_up     { actions.navigate     = Some(loc); }
                if let Some(loc) = select_dir { actions.select_dir   = Some(loc); }
                if let Some(loc) = play_loc   { actions.play         = Some(loc); }
            }
        }
    });

    actions
}

const BROWSER_LIST_ID: u64 = 0xB0_55_E5;
