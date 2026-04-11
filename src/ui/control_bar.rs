/// Persistent state for the control bar panel.
pub struct ControlBarState {
    pub video_name: String,
    pub is_playing: bool,
    /// Current playback position in seconds.
    pub current_secs: f64,
    /// Total duration in seconds; 0 if not yet known.
    pub duration_secs: f64,
    /// Index into SPEEDS.
    pub speed_index: usize,
    pub loop_active: bool,
}

pub const SPEEDS: [f32; 5] = [1.0, 2.0 / 3.0, 0.5, 1.0 / 3.0, 0.25];
pub const SPEED_LABELS: [&str; 5] = ["1x", "2/3x", "1/2x", "1/3x", "1/4x"];

impl Default for ControlBarState {
    fn default() -> Self {
        Self {
            video_name: String::new(),
            is_playing: true,
            current_secs: 0.0,
            duration_secs: 0.0,
            speed_index: 0,
            loop_active: false,
        }
    }
}

/// Actions that the control bar UI wants the app to perform.
/// Populated by `draw` and consumed by the caller.
#[derive(Default)]
pub struct ControlBarActions {
    pub play_pause: bool,
    pub prev: bool,
    pub next: bool,
    pub cycle_speed: bool,
    pub cycle_loop: bool,
    pub show_settings: bool,
    pub show_browser: bool,
    pub exit: bool,
    /// Seek target in [0, 1] (fraction of duration).
    pub seek_frac: Option<f32>,
}

pub fn draw(ui: &mut egui::Ui, state: &ControlBarState) -> ControlBarActions {
    let mut actions = ControlBarActions::default();

    // Slightly larger text for VR legibility.
    let font_id = egui::FontId::proportional(22.0);
    let btn_style = egui::TextStyle::Button;

    // ── icon row ──────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.style_mut().text_styles.insert(btn_style.clone(), font_id.clone());
        ui.spacing_mut().button_padding = egui::vec2(12.0, 8.0);

        if ui.button("◀◀").clicked() { actions.prev = true; }

        let play_label = if state.is_playing { "⏸" } else { "▶" };
        if ui.button(play_label).clicked() { actions.play_pause = true; }

        if ui.button("▶▶").clicked() { actions.next = true; }

        let speed_label = SPEED_LABELS[state.speed_index];
        if ui.button(speed_label).clicked() { actions.cycle_speed = true; }

        let loop_label = if state.loop_active { "↩●" } else { "↩" };
        if ui.button(loop_label).clicked() { actions.cycle_loop = true; }

        if ui.button("⚙").clicked() { actions.show_settings = true; }

        if ui.button("≡").clicked() { actions.show_browser = true; }

        if ui.button("✕").clicked() { actions.exit = true; }
    });

    // ── video name ────────────────────────────────────────────────────────
    ui.label(
        egui::RichText::new(state.video_name.as_str())
            .font(font_id.clone())
            .color(egui::Color32::WHITE),
    );

    // ── progress bar / scrubber ───────────────────────────────────────────
    ui.horizontal(|ui| {
        let time_label = format!(
            "{} / {}",
            fmt_time(state.current_secs),
            if state.duration_secs > 0.0 { fmt_time(state.duration_secs) } else { "--:--".into() }
        );

        let frac = if state.duration_secs > 0.0 {
            (state.current_secs / state.duration_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let bar = egui::ProgressBar::new(frac).text(time_label).desired_width(ui.available_width());
        ui.add(bar);
    });

    actions
}

fn fmt_time(secs: f64) -> String {
    let s = secs as u64;
    let m = s / 60;
    let h = m / 60;
    if h > 0 {
        format!("{h}:{:02}:{:02}", m % 60, s % 60)
    } else {
        format!("{m}:{:02}", s % 60)
    }
}
