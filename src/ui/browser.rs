use std::path::PathBuf;

pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mkv"];

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

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut BrowserState,
    interaction: Option<(egui::Pos2, egui::Pos2)>,
) -> BrowserActions {
    use super::icons;
    use super::ResponseExt as _;

    // Check if an async remote load completed this frame.
    state.poll_loading();

    let mut actions = BrowserActions::default();

    let font    = egui::FontId::proportional(22.0);
    let btn_pad = egui::vec2(10.0, 6.0);

    // Pre-extract everything we need before the borrow on state.entries begins.
    let scroll_to_current              = state.needs_scroll_to_current;
    state.needs_scroll_to_current      = false;
    let current_video                  = state.current_video.clone();
    let parent_loc                     = state.location.parent();
    let dir_display                    = state.location.display();

    // Make the scrollbar wide and always-visible so it's easy to grab in VR.
    ui.style_mut().spacing.scroll.bar_width = 20.0;
    ui.style_mut().spacing.scroll.floating  = false;

    // ── header: path + volumes button + close ────────────────────────────────
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 40.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
        ui.spacing_mut().button_padding = btn_pad;

        ui.label(
            egui::RichText::new(dir_display)
                .font(font.clone())
                .color(egui::Color32::LIGHT_GRAY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().button_padding = egui::vec2(6.0, 6.0);

            // Close button (rightmost).
            if icons::icon_button(ui, icons::ICON_CLOSE, 24.0, interaction) {
                actions.close = true;
            }

            // Volumes (drive picker) ComboBox — only shown for local directories.
            if state.location.as_local().is_some() {
                let mut vol_selected: Option<PathBuf> = None;
                egui::ComboBox::from_id_salt("volumes_picker")
                    .selected_text("Volumes")
                    .show_ui(ui, |ui| {
                        let vols = crate::volumes::list_volumes();
                        if vols.is_empty() {
                            ui.label("(no volumes found)");
                        } else {
                            for vol in &vols {
                                if ui.selectable_label(false, &vol.label).clicked() {
                                    vol_selected = Some(vol.root.clone());
                                }
                            }
                        }
                    });
                if let Some(root) = vol_selected {
                    actions.select_volume = Some(root);
                }
            }
        });
    });
    ui.separator();

    // ── loading / error states ────────────────────────────────────────────────
    if state.is_loading {
        ui.label(
            egui::RichText::new("Loading…")
                .font(font.clone())
                .color(egui::Color32::from_rgb(150, 200, 255)),
        );
        return actions;
    }
    if let Some(ref err) = state.load_error.clone() {
        ui.label(
            egui::RichText::new(format!("Error: {err}"))
                .font(font.clone())
                .color(egui::Color32::RED),
        );
        return actions;
    }

    // ── scrollable file / folder list ─────────────────────────────────────────

    let row_h     = 34.0_f32;
    let row_total = row_h + 2.0;

    let viewport_h = ui.available_height();

    let initial_offset: Option<f32> = if scroll_to_current {
        state.entries.iter().position(|e| {
            matches!(e, BrowserEntry::Video(_, loc) if current_video.as_ref() == Some(loc))
        }).map(|idx| {
            (idx as f32 * row_total - viewport_h / 2.0 + row_h / 2.0).max(0.0)
        })
    } else {
        None
    };

    let scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let scroll_area = match initial_offset {
        Some(off) => scroll_area.vertical_scroll_offset(off),
        None      => scroll_area,
    };

    scroll_area.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;

        let clip         = ui.clip_rect();
        let scroll_layer = ui.layer_id();
        let clipped_interaction = interaction.filter(|(press, release)| {
            clip.contains(*press) && clip.contains(*release)
                && ui.ctx().layer_id_at(*press) .map_or(true, |l| l == scroll_layer)
                && ui.ctx().layer_id_at(*release).map_or(true, |l| l == scroll_layer)
        });

        let hover_bg  = ui.visuals().widgets.hovered.weak_bg_fill;
        let select_bg = egui::Color32::from_rgb(40, 60, 90);
        let pad_x     = btn_pad.x;

        for entry in &state.entries {
            let w = ui.available_width();
            // (display, is_selected, text_color, navigate_up, select_dir, play)
            let (display, is_selected, text_color, navigate_up, select_dir, play_loc):
                (String, bool, egui::Color32, Option<Location>, Option<Location>, Option<Location>) = match entry
            {
                BrowserEntry::Parent => (
                    "..".to_string(),
                    false,
                    egui::Color32::WHITE,
                    parent_loc.clone(), // go up — use cache
                    None,
                    None,
                ),
                BrowserEntry::Dir(name, loc) => (
                    format!("📁  {name}"),
                    false,
                    egui::Color32::WHITE,
                    None,
                    Some(loc.clone()), // explicit folder click — invalidate cache
                    None,
                ),
                BrowserEntry::Video(name, loc) => {
                    let is_cur = current_video.as_ref() == Some(loc);
                    let color  = if is_cur { egui::Color32::YELLOW } else { egui::Color32::WHITE };
                    (name.clone(), is_cur, color, None, None, Some(loc.clone()))
                }
            };

            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(w, row_h),
                egui::Sense::click() | egui::Sense::hover(),
            );
            if ui.is_rect_visible(rect) {
                let bg = if resp.hovered() {
                    hover_bg
                } else if is_selected {
                    select_bg
                } else {
                    egui::Color32::TRANSPARENT
                };
                if bg != egui::Color32::TRANSPARENT {
                    ui.painter().rect_filled(rect, 2.0, bg);
                }
                ui.painter().text(
                    egui::pos2(rect.min.x + pad_x, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &display,
                    font.clone(),
                    text_color,
                );
            }
            if resp.activated_by(clipped_interaction) {
                if let Some(loc) = navigate_up { actions.navigate    = Some(loc); }
                if let Some(loc) = select_dir  { actions.select_dir  = Some(loc); }
                if let Some(loc) = play_loc    { actions.play        = Some(loc); }
            }
        }
    });

    actions
}
