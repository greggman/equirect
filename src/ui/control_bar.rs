use super::draw_list::Color;
use super::icons;
use super::ui::{
    FONT_DEFAULT, FONT_TINY, ITEM_SPACING_X, TEXT, TEXT_MUTED, Ui,
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

pub const ICON_SIZE:    f32 = 56.0;
/// Total width of one icon button: icon size + horizontal padding from
/// `ui.rs::icon_button` (8 px each side).
pub const ICON_BTN_W:   f32 = ICON_SIZE + 16.0;
/// Number of icon buttons in the row (matches the calls in `draw`).
pub const ICON_COUNT:   usize = 8;
const RED:              Color = Color { r: 1.0, g: 0.30, b: 0.30, a: 1.0 };

/// Exact panel-content dimensions implied by the icon row + label/slider rows.
/// Exposed so `app.rs` can size the panel texture and world quad to match.
pub const CONTROL_BAR_CONTENT_W: f32 =
    ICON_COUNT as f32 * ICON_BTN_W + (ICON_COUNT as f32 - 1.0) * ITEM_SPACING_X;
/// Icon row (56 + 6+6 pad_y) + spacing + title row (32) + spacing + slider row (48).
pub const CONTROL_BAR_CONTENT_H: f32 = (ICON_SIZE + 12.0) + 6.0 + 32.0 + 6.0 + 48.0;

pub fn draw(ui: &mut Ui, state: &ControlBarState) -> ControlBarActions {
    let mut actions = ControlBarActions::default();

    // ── icon row (no inter-button gap to match toolbar look) ──────────────
    ui.horizontal(|ui| {
        // The panel content area is sized to exactly fit this row, so any
        // residual slack from rounding is harmless — distribute it evenly.
        let slack = (ui.available_width() - CONTROL_BAR_CONTENT_W).max(0.0);
        ui.add_space(slack * 0.5);

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

    // ── video name / error  +  app version (centered together) ────────────
    ui.horizontal_h(32.0, |ui| {
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

    // ── seek scrubber + time (centered together at slider height) ─────────
    let time_label = format!(
        "{} / {}",
        fmt_time(state.current_secs),
        if state.duration_secs > 0.0 { fmt_time(state.duration_secs) } else { "--:--".into() }
    );

    let mut frac = if state.duration_secs > 0.0 {
        (state.current_secs / state.duration_secs).clamp(0.0, 1.0) as f32
    } else { 0.0 };

    // 48 = slider height — putting the row at that height vertically centers
    // the (shorter) time label with the (taller) slider.
    ui.horizontal_h(48.0, |ui| {
        // Reserve exactly the time label's pixel width on the right edge,
        // then let the slider take everything to the left of it.
        let time_w   = ui.measure_text(&time_label, FONT_DEFAULT);
        let gap      = 12.0;
        let slider_w = (ui.available_width() - time_w - gap).max(50.0);
        if ui.slider_sized(slider_w, &mut frac, 0.0, 1.0) {
            actions.seek_frac = Some(frac);
        }
        // horizontal_rtl takes the remaining row width and pins its contents
        // to the right edge, so the time label always sits flush right.
        ui.horizontal_rtl(|ui| {
            ui.label_styled(&time_label, FONT_DEFAULT, TEXT);
        });
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
