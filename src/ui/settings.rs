use super::icons::{self, IconSprite};
use super::ui::{FONT_DEFAULT, FONT_SMALL, TEXT, TEXT_DISABLED, TEXT_MUTED, Ui};

/// Video geometry mode.
#[derive(Clone, Copy, PartialEq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum VideoMode {
    #[default]
    Flat2D,
    Curved2D,
    Sbs3D,
    View180,
    View360,
}

/// Projection type (only meaningful for 180 / 360).
#[derive(Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum Projection {
    #[default]
    Equirect,
    Fisheye,
}

/// Stereo layout (meaningful for 3D / 180 / 360).
#[derive(Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum StereoLayout {
    #[default]
    OneView,
    LR,
    RL,
    TB,
    BT,
}

/// Persistent settings that affect how the video is rendered.
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoSettings {
    pub mode:   VideoMode,
    pub proj:   Projection,
    pub stereo: StereoLayout,
    pub zoom:   f32,
}

impl VideoSettings {
    pub fn new() -> Self {
        Self { zoom: 1.0, ..Default::default() }
    }
}

/// Actions the settings UI wants the app to perform.
#[derive(Default)]
pub struct SettingsActions {
    pub close: bool,
    pub changed: bool,
}

const ICON_SIZE: f32 = 96.0;

pub fn draw(ui: &mut Ui, state: &mut VideoSettings) -> SettingsActions {
    let mut actions = SettingsActions::default();

    let proj_enabled   = matches!(state.mode, VideoMode::View180 | VideoMode::View360);
    let stereo_enabled = matches!(state.mode, VideoMode::Sbs3D | VideoMode::View180 | VideoMode::View360);

    // ── header (title left, close right) ─────────────────────────────────────
    ui.horizontal_h(44.0, |ui| {
        ui.label_styled("Settings", FONT_DEFAULT + 2.0, TEXT);
        ui.horizontal_rtl(|ui| {
            if ui.icon_button(icons::ICON_CLOSE, 28.0, false) {
                actions.close = true;
            }
        });
    });
    ui.separator();

    // ── 1. Video mode ─────────────────────────────────────────────────────────
    section_label(ui, "Video Mode", true);
    ui.horizontal(|ui| {
        let icon_modes: &[(IconSprite, VideoMode)] = &[
            (icons::ICON_PLANE,      VideoMode::Flat2D),
            (icons::ICON_CURVE,      VideoMode::Curved2D),
            (icons::ICON_3D,         VideoMode::Sbs3D),
            (icons::ICON_HEMISPHERE, VideoMode::View180),
            (icons::ICON_SPHERE,     VideoMode::View360),
        ];
        for &(icon, mode) in icon_modes {
            let selected = state.mode == mode;
            if ui.icon_button(icon, ICON_SIZE, selected) && !selected {
                state.mode = mode;
                if !matches!(state.mode, VideoMode::View180 | VideoMode::View360) {
                    state.proj = Projection::default();
                }
                if !matches!(state.mode, VideoMode::Sbs3D | VideoMode::View180 | VideoMode::View360) {
                    state.stereo = StereoLayout::default();
                }
                actions.changed = true;
            }
        }
    });
    ui.add_space(8.0);

    // ── 2. Projection ─────────────────────────────────────────────────────────
    section_label(ui, "Projection", proj_enabled);
    ui.disabled(!proj_enabled, |ui| {
        ui.horizontal(|ui| {
            let projs: &[(IconSprite, Projection)] = &[
                (icons::ICON_EQUIRECT, Projection::Equirect),
                (icons::ICON_FISHEYE,  Projection::Fisheye),
            ];
            for &(icon, proj) in projs {
                let selected = state.proj == proj;
                if ui.icon_button(icon, ICON_SIZE, selected) && !selected {
                    state.proj = proj;
                    actions.changed = true;
                }
            }
        });
    });
    ui.add_space(8.0);

    // ── 3. Stereo layout ──────────────────────────────────────────────────────
    section_label(ui, "Stereo Layout", stereo_enabled);
    ui.disabled(!stereo_enabled, |ui| {
        ui.horizontal(|ui| {
            // "One View" only makes sense for 180/360 (not plain 3D).
            let one_view_ok = matches!(state.mode, VideoMode::View180 | VideoMode::View360);
            let layouts: &[(IconSprite, StereoLayout, bool)] = &[
                (icons::ICON_ONEVIEW, StereoLayout::OneView, one_view_ok),
                (icons::ICON_LR,      StereoLayout::LR,      true),
                (icons::ICON_RL,      StereoLayout::RL,      true),
                (icons::ICON_TB,      StereoLayout::TB,      true),
                (icons::ICON_BT,      StereoLayout::BT,      true),
            ];
            for &(icon, layout, avail) in layouts {
                let selected = state.stereo == layout;
                let pressed = ui.disabled(!avail, |ui| {
                    ui.icon_button(icon, ICON_SIZE, selected)
                });
                if pressed && !selected && avail {
                    state.stereo = layout;
                    actions.changed = true;
                }
            }
        });
    });
    ui.add_space(8.0);

    // ── 4. Zoom ───────────────────────────────────────────────────────────────
    section_label(ui, "Zoom", true);
    ui.horizontal_h(48.0, |ui| {
        if ui.button("1×") && (state.zoom - 1.0).abs() > 0.001 {
            state.zoom = 1.0;
            actions.changed = true;
        }
        let zoom_label = format!("{:.2}×", state.zoom);
        let label_w    = ui.measure_text(&zoom_label, FONT_DEFAULT) + 12.0;
        let slider_w   = (ui.available_width() - label_w - 8.0).max(50.0);
        if ui.slider_sized(slider_w, &mut state.zoom, 0.25, 4.0) {
            actions.changed = true;
        }
        ui.label_styled(&zoom_label, FONT_DEFAULT, TEXT);
    });

    actions
}

fn section_label(ui: &mut Ui, text: &str, enabled: bool) {
    let color = if enabled { TEXT_MUTED } else { TEXT_DISABLED };
    ui.label_styled(text, FONT_SMALL, color);
}
