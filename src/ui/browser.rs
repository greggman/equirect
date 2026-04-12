use std::path::PathBuf;

pub const VIDEO_EXTS: &[&str] = &["mp4", "m4v", "mkv"];

#[derive(Clone)]
pub enum BrowserEntry {
    Parent,
    Dir(String, PathBuf),
    Video(String, PathBuf),
}

pub struct BrowserState {
    pub dir: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub current_video: Option<PathBuf>,
    /// Set to true after creation or navigation; causes the list to scroll to
    /// the current video on the next draw.
    pub needs_scroll_to_current: bool,
}

/// Actions the browser UI wants the app to perform.
#[derive(Default)]
pub struct BrowserActions {
    pub play:     Option<PathBuf>,
    pub navigate: Option<PathBuf>,
    pub close:    bool,
}

impl BrowserState {
    pub fn new(dir: PathBuf, current_video: Option<PathBuf>) -> Self {
        let mut s = Self {
            dir,
            entries: Vec::new(),
            current_video,
            needs_scroll_to_current: true,
        };
        s.refresh();
        s
    }

    pub fn navigate_to(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.needs_scroll_to_current = false; // no current video to centre on in a new dir
        self.refresh();
    }

    pub fn refresh(&mut self) {
        self.entries.clear();
        self.entries.push(BrowserEntry::Parent);

        let mut dirs: Vec<(String, PathBuf)>   = Vec::new();
        let mut videos: Vec<(String, PathBuf)> = Vec::new();

        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    dirs.push((name, path));
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if VIDEO_EXTS.iter().any(|&v| v.eq_ignore_ascii_case(ext)) {
                        videos.push((name, path));
                    }
                }
            }
        }

        dirs.sort_by(|(a, _), (b, _)| a.cmp(b));
        videos.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (name, path) in dirs   { self.entries.push(BrowserEntry::Dir(name, path));   }
        for (name, path) in videos { self.entries.push(BrowserEntry::Video(name, path)); }
    }
}

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut BrowserState,
    interaction: Option<(egui::Pos2, egui::Pos2)>,
) -> BrowserActions {
    use super::ResponseExt as _;
    let mut actions = BrowserActions::default();

    let font    = egui::FontId::proportional(22.0);
    let btn_pad = egui::vec2(10.0, 6.0);

    // Pre-extract everything we need before the borrow on state.entries begins.
    let scroll_to_current             = state.needs_scroll_to_current;
    state.needs_scroll_to_current     = false;
    let current_video                 = state.current_video.clone();
    let parent_dir                    = state.dir.parent().map(|p| p.to_path_buf());
    let dir_display                   = state.dir.display().to_string();

    // Make the scrollbar wide and always-visible so it's easy to grab in VR.
    ui.style_mut().spacing.scroll.bar_width = 20.0;
    ui.style_mut().spacing.scroll.floating  = false;

    // ── header: path + Cancel ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;

        ui.label(
            egui::RichText::new(dir_display)
                .font(egui::FontId::proportional(16.0))
                .color(egui::Color32::LIGHT_GRAY),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Cancel").activated_by(interaction) {
                actions.close = true;
            }
        });
    });
    ui.separator();

    // ── scrollable file / folder list ─────────────────────────────────────

    // Each row is row_h px tall + item_spacing_y gap = row_total px in the content.
    let row_h     = 34.0_f32;
    let row_total = row_h + 2.0; // matches item_spacing.y set below

    // Compute initial scroll offset so the current video is already centred on
    // the very first frame — no disorienting jump.  Only applied when the flag
    // is true (first draw after open or navigate).
    let viewport_h = ui.available_height();

    let initial_offset: Option<f32> = if scroll_to_current {
        state.entries.iter().position(|e| {
            matches!(e, BrowserEntry::Video(_, p)
                if current_video.as_deref() == Some(p.as_path()))
        }).map(|idx| {
            // Centre the item inside the visible viewport.
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

        let hover_bg  = ui.visuals().widgets.hovered.weak_bg_fill;
        let select_bg = egui::Color32::from_rgb(40, 60, 90);
        let pad_x     = btn_pad.x;

        for entry in &state.entries {
            let w = ui.available_width();
            let (display, is_selected, text_color, navigate_to, play_path) = match entry {
                BrowserEntry::Parent => (
                    "..".to_string(), false, egui::Color32::WHITE,
                    parent_dir.clone(), None,
                ),
                BrowserEntry::Dir(name, path) => (
                    format!("📁  {name}"), false, egui::Color32::WHITE,
                    Some(path.clone()), None,
                ),
                BrowserEntry::Video(name, path) => {
                    let is_cur = current_video.as_deref() == Some(path.as_path());
                    let color  = if is_cur { egui::Color32::YELLOW } else { egui::Color32::WHITE };
                    (name.clone(), is_cur, color, None, Some(path.clone()))
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
            if resp.activated_by(interaction) {
                if let Some(p) = navigate_to { actions.navigate = Some(p); }
                if let Some(p) = play_path   { actions.play     = Some(p); }
            }
        }
    });

    actions
}
