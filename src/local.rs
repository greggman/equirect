use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};

use crate::input::ControllerState;
use crate::mode::Mode;
use crate::pointer_renderer::PointerRenderer;
use crate::renderer::Renderer;
use crate::ui::assets::UiAssets;
use crate::ui::browser::{BrowserActions, BrowserState, draw as browser_draw};
use crate::ui::control_bar::{ControlBarActions, ControlBarState};
use crate::ui::panel::PanelRenderer;
use crate::ui::settings::{SettingsActions, VideoSettings, draw as settings_draw};
use crate::video::texture::VideoTexture;
use crate::video_layer::VideoSwapchain;
use crate::video_renderer::VideoRenderer;

/// Vertical FOV used by the desktop projection.
const FOV_Y: f32 = 60.0_f32 * std::f32::consts::PI / 180.0;
/// Mouse sensitivity for view rotation (radians per pixel).
const MOUSE_SENS: f32 = 0.005;
/// Pitch limit so the camera never crosses the poles.
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 * 0.95;

/// Desktop-only render+input mode.
///
/// - Renders the equirectangular video to the window surface using the same
///   shader as VR mode, with mouse-drag (right button) controlling the camera
///   yaw/pitch.
/// - Renders UI panels at (0, 0, -2) in camera space — i.e. always centred on
///   screen regardless of view rotation — so the mouse can interact with them
///   without having to "aim" the camera.
/// - The mouse cursor is mapped to a single virtual controller; left-click is
///   the trigger.  Esc exits, M toggles the menu.
pub struct LocalMode {
    surface_format: wgpu::TextureFormat,

    yaw:   f32,
    pitch: f32,

    mouse_pos: PhysicalPosition<f64>,
    rotating:  bool,
    clicking:  bool,

    /// Window size in physical pixels — used to map mouse → ray and to size
    /// the projection matrix.
    window_w: u32,
    window_h: u32,

    /// Edge-triggered: M key press, consumed once per render_frame.
    menu_toggle_pending: bool,
    /// Set by Esc; surfaced via `exit_requested()`.
    exit_pending: bool,
}

impl LocalMode {
    pub fn new(renderer: &Renderer) -> Self {
        let size = renderer.window().inner_size();
        Self {
            surface_format: renderer.surface_format(),
            yaw:   0.0,
            pitch: 0.0,
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            rotating:  false,
            clicking:  false,
            window_w:  size.width.max(1),
            window_h:  size.height.max(1),
            menu_toggle_pending: false,
            exit_pending:        false,
        }
    }

    fn camera_orientation(&self) -> glam::Quat {
        glam::Quat::from_rotation_y(self.yaw) * glam::Quat::from_rotation_x(self.pitch)
    }

    /// View matrix used by the video shader: rotates the world by the inverse
    /// of the camera so the video sphere appears to spin around the viewer.
    fn video_view(&self) -> glam::Mat4 {
        glam::Mat4::from_quat(self.camera_orientation()).inverse()
    }

    fn projection(&self) -> glam::Mat4 {
        let aspect = self.window_w as f32 / self.window_h as f32;
        glam::Mat4::perspective_rh(FOV_Y, aspect, 0.1, 100.0)
    }

    /// Build the virtual controller for the current mouse pose.  Panels render
    /// HUD-style (view = identity), so the cursor ray lives in the same frame:
    /// origin at (0, 0, 0), direction through the mouse pixel.
    fn cursor_controller(&self) -> ControllerState {
        let (w, h) = (self.window_w as f32, self.window_h as f32);
        let nx = (self.mouse_pos.x as f32 / w) * 2.0 - 1.0;
        let ny = 1.0 - (self.mouse_pos.y as f32 / h) * 2.0;
        let aspect   = w / h;
        let tan_half = (FOV_Y * 0.5).tan();
        let dir = glam::Vec3::new(nx * tan_half * aspect, ny * tan_half, -1.0).normalize();
        ControllerState {
            ray_origin:   glam::Vec3::ZERO,
            ray_dir:      dir,
            clicking:     self.clicking,
            thumbstick_x: 0.0,
            thumbstick_y: 0.0,
            menu_pressed: false,
            grip_pressed: false,
        }
    }

    /// Clear the swapchain image to black.  Used when no video is loaded so
    /// the panel composite has a defined background instead of garbage.
    fn clear_to_black(
        target: &wgpu::TextureView,
        device: &wgpu::Device,
        queue:  &wgpu::Queue,
    ) {
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        queue.submit([encoder.finish()]);
    }
}

impl Mode for LocalMode {
    fn target_format(&self) -> wgpu::TextureFormat { self.surface_format }
    fn renders_to_window(&self) -> bool { true }
    fn exit_requested(&self) -> bool { self.exit_pending }

    // All panels render at (0, 0, -2) so they're always centred on screen.
    fn browser_position(&self)  -> glam::Vec3 { glam::Vec3::new(0.0, 0.0, -2.0) }
    fn settings_position(&self) -> glam::Vec3 { glam::Vec3::new(0.0, 0.0, -2.0) }

    fn handle_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if self.rotating {
                    let dx = (position.x - self.mouse_pos.x) as f32;
                    let dy = (position.y - self.mouse_pos.y) as f32;
                    self.yaw   -= dx * MOUSE_SENS;
                    self.pitch -= dy * MOUSE_SENS;
                    self.pitch = self.pitch.clamp(-PITCH_LIMIT, PITCH_LIMIT);
                }
                self.mouse_pos = *position;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = *state == ElementState::Pressed;
                match button {
                    MouseButton::Left  => self.clicking = pressed,
                    MouseButton::Right => self.rotating = pressed,
                    _ => {}
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed && !event.repeat {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        match code {
                            KeyCode::KeyM   => self.menu_toggle_pending = true,
                            KeyCode::Escape => self.exit_pending        = true,
                            _ => {}
                        }
                    }
                }
            }
            WindowEvent::Resized(size) => {
                self.window_w = size.width.max(1);
                self.window_h = size.height.max(1);
            }
            _ => {}
        }
    }

    fn render_frame(
        &mut self,
        renderer:          &Renderer,
        ui_assets:         &mut UiAssets,
        _video_layer:      Option<(&mut VideoSwapchain, &VideoSettings)>,
        video_shader:      Option<(&VideoRenderer, &VideoTexture, &VideoSettings)>,
        mut panel:         Option<(&mut PanelRenderer, &ControlBarState)>,
        mut browser:       Option<(&mut PanelRenderer, &mut BrowserState)>,
        mut settings:      Option<(&mut PanelRenderer, &mut VideoSettings)>,
        _pointer_renderer: Option<&PointerRenderer>,
    ) -> (ControlBarActions, BrowserActions, SettingsActions) {
        let menu_toggle = std::mem::take(&mut self.menu_toggle_pending);

        let cursor      = self.cursor_controller();
        let controllers = [Some(cursor), None];

        // Update UI textures (CPU-side draw build + GPU upload).
        let mut cb_actions = match &mut panel {
            Some((p, s)) => p.update(&renderer.device, &renderer.queue, ui_assets, s, &controllers),
            None => ControlBarActions::default(),
        };
        cb_actions.menu_toggle = menu_toggle;

        let browser_actions = match &mut browser {
            Some((p, s)) => p.update_ui(
                &renderer.device, &renderer.queue, ui_assets, &controllers,
                |ui| browser_draw(ui, s),
            ),
            None => BrowserActions::default(),
        };
        let settings_actions = match &mut settings {
            Some((p, s)) => p.update_ui(
                &renderer.device, &renderer.queue, ui_assets, &controllers,
                |ui| settings_draw(ui, s),
            ),
            None => SettingsActions::default(),
        };

        let proj       = self.projection();
        let video_view = self.video_view();
        let panel_view = glam::Mat4::IDENTITY;
        let device     = &renderer.device;
        let queue      = &renderer.queue;

        renderer.render_with(|target, _w, _h| {
            // 1. Video — first pass clears the surface.
            if let Some((vr_rend, _, vs)) = video_shader {
                vr_rend.render_eye(target, proj, video_view, vs, 0, device, queue);
            } else {
                Self::clear_to_black(target, device, queue);
            }

            // 2. Panels composite on top (LoadOp::Load inside render_eye).
            if let Some((p, _)) = &panel {
                p.render_eye(target, proj, panel_view, device, queue);
            }
            if let Some((p, _)) = &browser {
                p.render_eye(target, proj, panel_view, device, queue);
            }
            if let Some((p, _)) = &settings {
                p.render_eye(target, proj, panel_view, device, queue);
            }
        });

        (cb_actions, browser_actions, settings_actions)
    }
}
