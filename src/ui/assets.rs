use super::atlas::GlyphAtlas;
use super::icons;
use super::renderer::UiRenderer;

/// GPU-resident UI assets shared by every panel: the glyph atlas, the icon
/// sprite sheet, and the wgpu pipeline that consumes a `DrawList`.
///
/// Lives in `App` and is passed to each panel's `update` so we don't duplicate
/// fonts and pipelines per panel.
pub struct UiAssets {
    pub atlas:        GlyphAtlas,
    pub renderer:     UiRenderer,
    /// Kept alive alongside `icon_view`.
    #[allow(dead_code)]
    pub icon_texture: wgpu::Texture,
    pub icon_view:    wgpu::TextureView,
}

/// Format every panel's offscreen UI texture uses.  The `UiRenderer` pipeline
/// is built for this exact format, so it must match the textures created in
/// `PanelRenderer::new`.
pub const PANEL_OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl UiAssets {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let (primary, fallbacks) = super::fonts::load_font_bytes();
        let atlas = GlyphAtlas::new(device, primary, fallbacks);
        let renderer = UiRenderer::new(device, PANEL_OFFSCREEN_FORMAT);
        let (icon_texture, icon_view) = icons::create_sheet_texture(device, queue);
        Self { atlas, renderer, icon_texture, icon_view }
    }
}
