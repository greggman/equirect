pub mod browser;
pub mod control_bar;
pub mod icons;
pub mod panel;
pub mod settings;

/// Pointer-capture semantics for VR controller buttons.
///
/// A widget activates if (a) the controller button was originally pressed while
/// the pointer was over it, and (b) the pointer is still over it at release.
/// Moving off cancels; moving onto a different widget does not steal the press.
///
/// `interaction` is `Some((press_pos, release_pos))` on the single release frame,
/// both in egui pixel space.  Both positions must fall inside the widget's rect.
/// This avoids relying on egui's `hovered()`, which can lag by a frame.
pub trait ResponseExt {
    fn activated_by(&self, interaction: Option<(egui::Pos2, egui::Pos2)>) -> bool;
}

impl ResponseExt for egui::Response {
    fn activated_by(&self, interaction: Option<(egui::Pos2, egui::Pos2)>) -> bool {
        interaction.map_or(false, |(press, release)| {
            self.rect.contains(press) && self.rect.contains(release)
        })
    }
}
