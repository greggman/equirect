/// Video geometry mode.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum VideoMode {
    #[default]
    Flat2D,
    Curved2D,
    Sbs3D,
    View180,
    View360,
}

/// Projection type (only meaningful for 180 / 360).
#[derive(Clone, Copy, PartialEq, Default)]
pub enum Projection {
    #[default]
    Equirect,
    Fisheye,
}

/// Stereo layout (meaningful for 3D / 180 / 360).
#[derive(Clone, Copy, PartialEq, Default)]
pub enum StereoLayout {
    #[default]
    OneView,
    LR,
    RL,
    TB,
    BT,
}

/// Persistent settings that affect how the video is rendered.
#[derive(Clone, Default, PartialEq)]
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
    just_released: bool,
) -> SettingsActions {
    let mut actions = SettingsActions::default();

    let font    = egui::FontId::proportional(22.0);
    let btn_pad = egui::vec2(10.0, 6.0);

    // Helper: whether projection section is relevant.
    let proj_enabled   = matches!(state.mode, VideoMode::View180 | VideoMode::View360);
    // Stereo is relevant for 3D, 180, 360.
    let stereo_enabled = matches!(state.mode, VideoMode::Sbs3D | VideoMode::View180 | VideoMode::View360);

    // ── header ───────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;

        ui.label(
            egui::RichText::new("Settings")
                .font(egui::FontId::proportional(24.0))
                .color(egui::Color32::WHITE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Cancel").hovered() && just_released {
                actions.close = true;
            }
        });
    });
    ui.separator();

    // ── section helper ───────────────────────────────────────────────────────
    // Draws a section heading then a horizontal row of radio-style buttons.
    // Returns true if any button was pressed.
    let section_label = |ui: &mut egui::Ui, text: &str, enabled: bool| {
        let color = if enabled { egui::Color32::LIGHT_GRAY } else { egui::Color32::DARK_GRAY };
        ui.label(egui::RichText::new(text).font(egui::FontId::proportional(16.0)).color(color));
    };

    // ── 1. Video mode ─────────────────────────────────────────────────────────
    section_label(ui, "Video Mode", true);
    ui.horizontal(|ui| {
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;

        let modes: &[(&str, VideoMode)] = &[
            ("2D",       VideoMode::Flat2D),
            ("2D Curve", VideoMode::Curved2D),
            ("3D",       VideoMode::Sbs3D),
            ("180",      VideoMode::View180),
            ("360",      VideoMode::View360),
        ];
        for &(label, mode) in modes {
            let selected = state.mode == mode;
            let btn = egui::Button::new(label).selected(selected);
            if ui.add(btn).hovered() && just_released && !selected {
                state.mode = mode;
                // Reset sub-options that may not be valid in new mode.
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
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;
        ui.horizontal(|ui| {
            let projs: &[(&str, Projection)] = &[
                ("Equirect", Projection::Equirect),
                ("Fisheye",  Projection::Fisheye),
            ];
            for &(label, proj) in projs {
                let selected = state.proj == proj;
                let btn = egui::Button::new(label).selected(selected);
                if ui.add(btn).hovered() && just_released && !selected && proj_enabled {
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
        ui.style_mut().text_styles.insert(egui::TextStyle::Button, font.clone());
        ui.spacing_mut().button_padding = btn_pad;
        ui.horizontal(|ui| {
            // "One View" only makes sense for 180/360 (not plain 3D).
            let one_view_ok = matches!(state.mode, VideoMode::View180 | VideoMode::View360);
            let layouts: &[(&str, StereoLayout, bool)] = &[
                ("One View", StereoLayout::OneView, one_view_ok),
                ("L/R",      StereoLayout::LR,      true),
                ("R/L",      StereoLayout::RL,      true),
                ("T/B",      StereoLayout::TB,      true),
                ("B/T",      StereoLayout::BT,      true),
            ];
            for &(label, layout, avail) in layouts {
                let selected = state.stereo == layout;
                let btn = egui::Button::new(label).selected(selected);
                let resp = ui.add_enabled(avail, btn);
                if resp.hovered() && just_released && !selected && stereo_enabled && avail {
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
        if ui.button("1×").hovered() && just_released && (state.zoom - 1.0).abs() > 0.001 {
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
