use super::draw_list::Color;
use super::icons;
use super::ui::{
    FONT_DEFAULT, FONT_TINY, TEXT, TEXT_MUTED, Ui,
};

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

const ICON_SIZE: f32 = 64.0;
const RED:   Color = Color { r: 1.0, g: 0.30, b: 0.30, a: 1.0 };

pub fn draw(ui: &mut Ui, state: &ControlBarState) -> ControlBarActions {
    let mut actions = ControlBarActions::default();

    // ── icon row ──────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.icon_button(icons::ICON_PREV, ICON_SIZE, false) { actions.prev = true; }

        let play_sprite = if state.is_playing { icons::ICON_PAUSE } else { icons::ICON_PLAY };
        if ui.icon_button(play_sprite, ICON_SIZE, false) { actions.play_pause = true; }

        if ui.icon_button(icons::ICON_NEXT, ICON_SIZE, false) { actions.next = true; }

        let speed_sprite = match state.speed_index {
            0 => icons::ICON_SPEED_1X,
            1 => icons::ICON_SPEED__66X,
            2 => icons::ICON_SPEED__5X,
            3 => icons::ICON_SPEED__33X,
            _ => icons::ICON_SPEED__25X,
        };
        if ui.icon_button(speed_sprite, ICON_SIZE, false) { actions.cycle_speed = true; }

        let loop_sprite = match state.loop_state {
            0 => icons::ICON_LOOP_0,
            1 => icons::ICON_LOOP_1,
            _ => icons::ICON_LOOP_2,
        };
        let loop_active = state.loop_state > 0;
        if ui.icon_button(loop_sprite, ICON_SIZE, loop_active) { actions.cycle_loop = true; }

        if ui.icon_button(icons::ICON_SETTINGS, ICON_SIZE, false) { actions.show_settings = true; }
        if ui.icon_button(icons::ICON_FOLDER,   ICON_SIZE, false) { actions.show_browser  = true; }
        if ui.icon_button(icons::ICON_EXIT,     ICON_SIZE, false) { actions.exit          = true; }
    });

    // ── video name / error  +  app version ────────────────────────────────
    ui.horizontal(|ui| {
        if let Some(err) = &state.error {
            ui.label_styled(err, FONT_DEFAULT, RED);
        } else {
            ui.label_styled(&state.video_name, FONT_DEFAULT, TEXT);
        }
        ui.horizontal_rtl(|ui| {
            ui.label_styled(
                concat!(env!("CARGO_PKG_NAME"), " v", env!("CARGO_PKG_VERSION")),
                FONT_TINY, TEXT_MUTED,
            );
        });
    });

    // ── seek scrubber + time (same row) ───────────────────────────────────
    let time_label = format!(
        "{} / {}",
        fmt_time(state.current_secs),
        if state.duration_secs > 0.0 { fmt_time(state.duration_secs) } else { "--:--".into() }
    );

    let mut frac = if state.duration_secs > 0.0 {
        (state.current_secs / state.duration_secs).clamp(0.0, 1.0) as f32
    } else { 0.0 };

    ui.horizontal(|ui| {
        // Reserve the right-side label width up front so the slider can use
        // the rest of the row.
        let time_w   = ui.measure_text(&time_label, FONT_DEFAULT) + 12.0;
        let slider_w = (ui.available_width() - time_w - 8.0).max(50.0);
        if ui.slider_sized(slider_w, &mut frac, 0.0, 1.0) {
            actions.seek_frac = Some(frac);
        }
        ui.label_styled(&time_label, FONT_DEFAULT, TEXT);
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
