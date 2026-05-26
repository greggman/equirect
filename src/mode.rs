use winit::event::WindowEvent;

use crate::pointer_renderer::PointerRenderer;
use crate::renderer::Renderer;
use crate::ui::assets::UiAssets;
use crate::ui::browser::{BrowserActions, BrowserState};
use crate::ui::control_bar::{ControlBarActions, ControlBarState};
use crate::ui::panel::PanelRenderer;
use crate::ui::settings::{SettingsActions, VideoSettings};
use crate::video::texture::VideoTexture;
use crate::video_layer::VideoSwapchain;
use crate::video_renderer::VideoRenderer;

/// A render+input mode for the application — either VR (OpenXR) or local desktop.
///
/// The trait keeps the rest of the app oblivious to which mode is active so we
/// don't have to scatter `if local { … } else { … }` through it.  Each mode
/// owns its own input source (XR controllers vs winit mouse/keyboard) and its
/// own render target (XR swapchains vs the desktop surface).
pub trait Mode {
    /// Texture format that panels and the video shader render to.
    fn target_format(&self) -> wgpu::TextureFormat;

    /// True when this mode can present video via an XR composition layer
    /// (quad / cylinder / equirect2).  Local mode always uses the shader path.
    fn supports_video_layer(&self) -> bool { false }

    /// True when the XR runtime advertises `XR_KHR_composition_layer_equirect2`.
    fn supports_equirect2(&self) -> bool { false }

    /// Allocate a video swapchain bound to the XR session.  Returns `None` for
    /// modes that don't use composition layers.
    fn create_video_swapchain(
        &self,
        _device: &wgpu::Device,
        _width:  u32,
        _height: u32,
    ) -> Option<VideoSwapchain> { None }

    /// Render one frame and return any user actions triggered this frame.
    #[allow(clippy::type_complexity)]
    fn render_frame(
        &mut self,
        renderer:         &Renderer,
        ui_assets:        &mut UiAssets,
        video_layer:      Option<(&mut VideoSwapchain, &VideoSettings)>,
        video_shader:     Option<(&VideoRenderer, &VideoTexture, &VideoSettings)>,
        panel:            Option<(&mut PanelRenderer, &ControlBarState)>,
        browser:          Option<(&mut PanelRenderer, &mut BrowserState)>,
        settings:         Option<(&mut PanelRenderer, &mut VideoSettings)>,
        pointer_renderer: Option<&PointerRenderer>,
    ) -> (ControlBarActions, BrowserActions, SettingsActions);

    /// True when the runtime has signalled session termination.
    fn should_quit(&self) -> bool { false }

    /// Ask the underlying runtime to exit cleanly.
    fn request_exit(&self) {}

    /// Forward a winit window event so the mode can update its input state.
    /// VR mode ignores this entirely.
    fn handle_window_event(&mut self, _event: &WindowEvent) {}

    /// True when the mode renders directly to the desktop window surface (i.e.
    /// the App should not also draw the logo there).
    fn renders_to_window(&self) -> bool { false }

    /// True when the user has asked the application to terminate (e.g. Esc in
    /// local mode).  Distinct from `should_quit`: `should_quit` only retires
    /// the mode while leaving the App alive, whereas this exits everything.
    fn exit_requested(&self) -> bool { false }

    // ── 3-D panel placement ───────────────────────────────────────────────────
    // Each mode chooses where panels appear in the world.  Defaults match the
    // VR placements; LocalMode overrides them so panels stay centred on screen.

    fn control_bar_position(&self) -> glam::Vec3 { glam::Vec3::new(0.0, 0.0, -2.0) }
    fn browser_position(&self)     -> glam::Vec3 { glam::Vec3::new(0.0, 1.2, -2.0) }
    fn settings_position(&self)    -> glam::Vec3 { glam::Vec3::new(0.0, 1.2, -2.0) }
}
