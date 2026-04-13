/// Persistent state for the control bar panel.
pub struct ControlBarState {
    pub video_name: String,
    pub error: Option<String>,
    pub is_playing: bool,
    /// Current playback position in seconds.
    pub current_secs: f64,
    /// Total duration in seconds; 0 if not yet known.
    pub duration_secs: f64,
    /// Index into SPEEDS.
    pub speed_index: usize,
    /// Loop state: 0 = off, 1 = start set, 2 = active (start + end set).
    pub loop_state: u8,
}

pub const SPEEDS: [f32; 5] = [1.0, 2.0 / 3.0, 0.5, 1.0 / 3.0, 0.25];

impl Default for ControlBarState {
    fn default() -> Self {
        Self {
            video_name: String::new(),
            error: None,
            is_playing: true,
            current_secs: 0.0,
            duration_secs: 0.0,
            speed_index: 0,
            loop_state: 0,
        }
    }
}

/// Actions that the control bar UI wants the app to perform.
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
    /// Thumbstick-driven seek: signed seconds to add to the current position
    /// (wraps at duration boundaries). Only emitted at most every 100 ms.
    pub seek_delta_secs: Option<f64>,
    /// B (right) / Y (left) button just pressed — toggle panel visibility.
    pub menu_toggle: bool,
}

/// `just_released` — true on the single frame the controller select button
/// went from pressed → released.  We use this instead of egui's `clicked()`
/// because egui's internal click-distance gating can silently drop clicks when
/// the VR controller tremors between press and release.
/// Returns true if `press_pos` falls inside `resp`'s rect — the definition of
/// pointer capture: only the widget the button was pressed on may fire.
pub fn draw(ui: &mut egui::Ui, state: &ControlBarState, interaction: Option<(egui::Pos2, egui::Pos2)>) -> ControlBarActions {
    use super::icons;
    let mut actions = ControlBarActions::default();

    let font_id = egui::FontId::proportional(22.0);

    // ── icon row ──────────────────────────────────────────────────────────
    const ICON_SIZE: f32 = 64.0;

    ui.horizontal(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(8.0, 6.0);

        if icons::icon_button(ui, icons::ICON_PREV, ICON_SIZE, interaction) { actions.prev = true; }

        let play_sprite = if state.is_playing { icons::ICON_PAUSE } else { icons::ICON_PLAY };
        if icons::icon_button(ui, play_sprite, ICON_SIZE, interaction) { actions.play_pause = true; }

        if icons::icon_button(ui, icons::ICON_NEXT, ICON_SIZE, interaction) { actions.next = true; }

        let speed_sprite = match state.speed_index {
            0 => icons::ICON_SPEED_1X,
            1 => icons::ICON_SPEED__66X,
            2 => icons::ICON_SPEED__5X,
            3 => icons::ICON_SPEED__33X,
            _ => icons::ICON_SPEED__25X,
        };
        if icons::icon_button(ui, speed_sprite, ICON_SIZE, interaction) { actions.cycle_speed = true; }

        let loop_sprite = match state.loop_state {
            0 => icons::ICON_LOOP_0,
            1 => icons::ICON_LOOP_1,
            _ => icons::ICON_LOOP_2,
        };
        let loop_active = state.loop_state > 0;
        if icons::icon_button_selected(ui, loop_sprite, loop_active, ICON_SIZE, interaction) { actions.cycle_loop = true; }

        if icons::icon_button(ui, icons::ICON_SETTINGS, ICON_SIZE, interaction) { actions.show_settings = true; }
        if icons::icon_button(ui, icons::ICON_FOLDER,   ICON_SIZE, interaction) { actions.show_browser = true; }
        if icons::icon_button(ui, icons::ICON_EXIT,     ICON_SIZE, interaction) { actions.exit = true; }
    });

    // ── video name / error ────────────────────────────────────────────────
    if let Some(ref err) = state.error {
        ui.label(
            egui::RichText::new(err.as_str())
                .font(font_id.clone())
                .color(egui::Color32::RED),
        );
    } else {
        ui.label(
            egui::RichText::new(state.video_name.as_str())
                .font(font_id.clone())
                .color(egui::Color32::WHITE),
        );
    }

    // ── seek scrubber + time (same row) ──────────────────────────────────
    let time_label = format!(
        "{} / {}",
        fmt_time(state.current_secs),
        if state.duration_secs > 0.0 { fmt_time(state.duration_secs) } else { "--:--".into() }
    );
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(
            egui::RichText::new(time_label)
                .font(font_id.clone())
                .color(egui::Color32::WHITE),
        );

        let mut frac = if state.duration_secs > 0.0 {
            (state.current_secs / state.duration_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let width = ui.available_width();
        ui.spacing_mut().slider_width = width;
        let resp = ui.add(
            egui::Slider::new(&mut frac, 0.0f32..=1.0f32)
                .show_value(false)
                .trailing_fill(true),
        );
        if resp.changed() {
            actions.seek_frac = Some(frac);
        }
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
