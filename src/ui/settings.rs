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

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut VideoSettings,
    interaction: Option<(egui::Pos2, egui::Pos2)>,
) -> SettingsActions {
    use super::icons;
    use super::icons::IconSprite;
    use super::ResponseExt as _;
    let mut actions = SettingsActions::default();

    let font    = egui::FontId::proportional(22.0);
    let btn_pad = egui::vec2(10.0, 6.0);

    // Helper: whether projection section is relevant.
    let proj_enabled   = matches!(state.mode, VideoMode::View180 | VideoMode::View360);
    // Stereo is relevant for 3D, 180, 360.
    let stereo_enabled = matches!(state.mode, VideoMode::Sbs3D | VideoMode::View180 | VideoMode::View360);

    // ── header ───────────────────────────────────────────────────────────────
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 40.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().button_padding = btn_pad;

            ui.label(
                egui::RichText::new("Settings")
                    .font(egui::FontId::proportional(24.0))
                    .color(egui::Color32::WHITE),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().button_padding = egui::vec2(6.0, 6.0);
                if icons::icon_button(ui, icons::ICON_CLOSE, 24.0, interaction) {
                    actions.close = true;
                }
            });
        },
    );
    ui.separator();

    // ── section helper ───────────────────────────────────────────────────────
    // Draws a section heading then a horizontal row of radio-style buttons.
    // Returns true if any button was pressed.
    let section_label = |ui: &mut egui::Ui, text: &str, enabled: bool| {
        let color = if enabled { egui::Color32::LIGHT_GRAY } else { egui::Color32::DARK_GRAY };
        ui.label(egui::RichText::new(text).font(egui::FontId::proportional(16.0)).color(color));
    };

    const ICON_SIZE: f32 = 96.0;

    // ── 1. Video mode ─────────────────────────────────────────────────────────
    section_label(ui, "Video Mode", true);
    ui.horizontal(|ui| {
        ui.spacing_mut().button_padding = btn_pad;

        let icon_modes: &[(IconSprite, VideoMode)] = &[
            (icons::ICON_PLANE,      VideoMode::Flat2D),
            (icons::ICON_CURVE,      VideoMode::Curved2D),
            (icons::ICON_3D,         VideoMode::Sbs3D),
            (icons::ICON_HEMISPHERE, VideoMode::View180),
            (icons::ICON_SPHERE,     VideoMode::View360),
        ];
        for &(icon, mode) in icon_modes {
            let selected = state.mode == mode;
            if icons::icon_button_selected(ui, icon, selected, ICON_SIZE, interaction) && !selected {
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
    ui.add_enabled_ui(proj_enabled, |ui| {
        ui.spacing_mut().button_padding = btn_pad;
        ui.horizontal(|ui| {
            let projs: &[(IconSprite, Projection)] = &[
                (icons::ICON_EQUIRECT, Projection::Equirect),
                (icons::ICON_FISHEYE,  Projection::Fisheye),
            ];
            for &(icon, proj) in projs {
                let selected = state.proj == proj;
                if icons::icon_button_selected(ui, icon, selected, ICON_SIZE, interaction) && !selected && proj_enabled {
                    state.proj = proj;
                    actions.changed = true;
                }
            }
        });
    });
    ui.add_space(8.0);

    // ── 3. Stereo layout ──────────────────────────────────────────────────────
    section_label(ui, "Stereo Layout", stereo_enabled);
    ui.add_enabled_ui(stereo_enabled, |ui| {
        ui.spacing_mut().button_padding = btn_pad;
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
                let resp = ui.add_enabled(
                    avail,
                    egui::Button::image(icons::icon_image(ui.ctx(), icon, ICON_SIZE))
                        .selected(selected),
                );
                if resp.activated_by(interaction) && !selected && stereo_enabled && avail {
                    state.stereo = layout;
                    actions.changed = true;
                }
            }
        });
    });
    ui.add_space(8.0);

    // ── 4. Zoom ───────────────────────────────────────────────────────────────
    section_label(ui, "Zoom", true);
    ui.horizontal(|ui| {
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;

        // Reset button
        if ui.button("1×").activated_by(interaction) && (state.zoom - 1.0).abs() > 0.001 {
            state.zoom = 1.0;
            actions.changed = true;
        }

        // Zoom slider: 0.25× – 4.0×
        let avail = ui.available_width();
        ui.spacing_mut().slider_width = avail;
        let resp = ui.add(
            egui::Slider::new(&mut state.zoom, 0.25_f32..=4.0_f32)
                .show_value(true)
                .custom_formatter(|v, _| format!("{v:.2}×")),
        );
        if resp.changed() {
            actions.changed = true;
        }
    });

    actions
}
