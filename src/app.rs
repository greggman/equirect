use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::pointer_renderer::PointerRenderer;
use crate::renderer::Renderer;
use crate::ui::browser::BrowserState;
use crate::ui::settings::VideoSettings;

#[derive(Clone, Copy, PartialEq)]
enum PanelMode {
    ControlBar,
    Browser,
    Settings,
    Hidden,
}
use crate::ui::control_bar::{ControlBarState, SPEEDS};
use crate::ui::panel::PanelRenderer;
use crate::video::decoder::VideoDecoder;
use crate::video::texture::VideoTexture;
use crate::video_renderer::VideoRenderer;
use crate::vr::{VrContext, VrPreInit};

pub struct App {
    // Drop order matters: vr before renderer so the XR session is destroyed
    // while the wgpu Vulkan device is still alive.
    vr: Option<VrContext>,
    renderer: Option<Renderer>,
    video_decoder: Option<VideoDecoder>,
    video_texture: Option<VideoTexture>,
    video_renderer: Option<VideoRenderer>,
    panel_renderer: Option<PanelRenderer>,
    pointer_renderer: Option<PointerRenderer>,
    control_bar_state: ControlBarState,
    video_path: Option<PathBuf>,
    // File browser
    browser_state: Option<BrowserState>,
    browser_panel: Option<PanelRenderer>,
    // Settings
    video_settings: VideoSettings,
    settings_panel: Option<PanelRenderer>,
    // Panel visibility
    panel_mode: PanelMode,
    /// Which mode to restore when B/Y un-hides; defaults to ControlBar.
    hidden_from: PanelMode,
}

impl App {
    pub fn new(video_path: Option<PathBuf>) -> Self {
        Self {
            vr: None,
            renderer: None,
            video_decoder: None,
            video_texture: None,
            video_renderer: None,
            panel_renderer: None,
            pointer_renderer: None,
            control_bar_state: ControlBarState::default(),
            video_path,
            browser_state: None,
            browser_panel: None,
            video_settings: VideoSettings::new(),
            settings_panel: None,
            panel_mode: PanelMode::ControlBar,
            hidden_from: PanelMode::ControlBar,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() {
            return;
        }

        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("vrust-v"))
                .expect("Failed to create window"),
        );

        let vr_pre = VrPreInit::new();
        let xr_exts = vr_pre.as_ref().map(|v| v.required_device_extensions()).unwrap_or_default();

        let mut renderer = Renderer::new(window, &xr_exts);

        let vr = vr_pre.and_then(|pre| VrContext::new(&renderer, pre));
        if let Some(ref vr) = vr {
            renderer.prepare_for_xr(vr.swapchain_format);
        }

        let target_fmt = vr
            .as_ref()
            .map(|v| v.swapchain_format)
            .unwrap_or_else(|| renderer.surface_format());

        if let Some(ref path) = self.video_path {
            match VideoDecoder::open(path.clone()) {
                Err(e) => eprintln!("Failed to open video: {e}"),
                Ok(decoder) => {
                    let tex = VideoTexture::new(&renderer.device, decoder.width, decoder.height);
                    let vr_rend = VideoRenderer::new(
                        &renderer.device,
                        target_fmt,
                        decoder.width,
                        decoder.height,
                    );

                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        self.control_bar_state.video_name = stem.to_owned();
                    }

                    // Seed duration if already known at open time.
                    if decoder.duration_us > 0 {
                        self.control_bar_state.duration_secs =
                            decoder.duration_us as f64 / 1_000_000.0;
                    }

                    self.video_decoder = Some(decoder);
                    self.video_texture = Some(tex);
                    self.video_renderer = Some(vr_rend);
                }
            }
        }

        // Panel: original 800×160 px canvas, displayed at 2× the original physical size
        // (2.0 m wide × 0.4 m tall) so everything inside appears 2× larger.
        // Centered at Y=0.0, just below the video whose bottom sits near Y=0.
        self.panel_renderer = Some(PanelRenderer::new(
            &renderer.device,
            target_fmt,
            800,
            160,
            glam::Vec3::new(0.0, 0.0, -2.0),
            2.0,
            0.4,
        ));

        self.pointer_renderer = Some(PointerRenderer::new(&renderer.device, target_fmt));

        self.renderer = Some(renderer);
        self.vr = vr;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer {
                    r.resize(size);
                }
            }

            WindowEvent::RedrawRequested => {
                let Some(r) = &mut self.renderer else { return };
                if !r.render() {
                    let size = r.window().inner_size();
                    r.resize(size);
                }
                r.window().request_redraw();
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        let Some(renderer) = &self.renderer else { return };

        // Sync duration once the decoder has it (it arrives async from the media source).
        if self.control_bar_state.duration_secs == 0.0 {
            if let Some(dec) = &self.video_decoder {
                if dec.duration_us > 0 {
                    self.control_bar_state.duration_secs =
                        dec.duration_us as f64 / 1_000_000.0;
                }
            }
        }

        // Upload the latest decoded frame if one is available.
        if let (Some(decoder), Some(texture), Some(vr_rend)) = (
            &self.video_decoder,
            &self.video_texture,
            &mut self.video_renderer,
        ) {
            if let Some(frame) = decoder.take_frame() {
                texture.upload(&renderer.queue, &frame.data);
                vr_rend.set_texture(&renderer.device, texture);
            }

            let pts_us = decoder.current_pts_us.load(Ordering::Relaxed);
            self.control_bar_state.current_secs = pts_us as f64 / 1_000_000.0;
        }

        if let Some(vr) = &mut self.vr {
            if vr.should_quit {
                renderer
                    .device
                    .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
                    .ok();
                self.vr = None;
                return;
            }

            let video = self.video_renderer.as_ref().zip(self.video_texture.as_ref());

            // Only pass the panel that should currently be visible.
            let panel_arg: Option<(&mut PanelRenderer, &ControlBarState)> =
                if self.panel_mode == PanelMode::ControlBar {
                    self.panel_renderer.as_mut().map(|p| (p, &self.control_bar_state))
                } else {
                    None
                };
            let browser_arg: Option<(&mut PanelRenderer, &mut BrowserState)> =
                if self.panel_mode == PanelMode::Browser {
                    self.browser_panel.as_mut().zip(self.browser_state.as_mut())
                } else {
                    None
                };

            let settings_arg: Option<(&mut PanelRenderer, &mut VideoSettings)> =
                if self.panel_mode == PanelMode::Settings {
                    self.settings_panel.as_mut().map(|p| (p, &mut self.video_settings))
                } else {
                    None
                };

            let (actions, browser_actions, settings_actions) = vr.render_frame(
                renderer,
                video,
                panel_arg,
                browser_arg,
                settings_arg,
                self.pointer_renderer.as_ref(),
            );

            // ── handle browser actions ─────────────────────────────────────

            if browser_actions.close {
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::ControlBar;
            }

            if let Some(dir) = browser_actions.navigate {
                if let Some(bs) = &mut self.browser_state {
                    bs.navigate_to(dir);
                }
            }

            if let Some(new_path) = browser_actions.play {
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::Hidden;
                self.hidden_from   = PanelMode::ControlBar;

                let target_fmt = vr.swapchain_format;

                // Drop old video components before opening the new file.
                self.video_decoder = None;
                self.video_texture  = None;
                self.video_renderer = None;

                match VideoDecoder::open(new_path.clone()) {
                    Err(e) => eprintln!("Failed to open video: {e}"),
                    Ok(decoder) => {
                        let tex = VideoTexture::new(
                            &renderer.device, decoder.width, decoder.height,
                        );
                        let vr_rend = VideoRenderer::new(
                            &renderer.device, target_fmt,
                            decoder.width, decoder.height,
                        );
                        if let Some(stem) = new_path.file_stem().and_then(|s| s.to_str()) {
                            self.control_bar_state.video_name = stem.to_owned();
                        }
                        if decoder.duration_us > 0 {
                            self.control_bar_state.duration_secs =
                                decoder.duration_us as f64 / 1_000_000.0;
                        } else {
                            self.control_bar_state.duration_secs = 0.0;
                        }
                        self.control_bar_state.current_secs = 0.0;
                        self.control_bar_state.is_playing    = true;
                        self.video_path    = Some(new_path);
                        self.video_decoder = Some(decoder);
                        self.video_texture = Some(tex);
                        self.video_renderer = Some(vr_rend);
                    }
                }
            }

            // ── handle settings actions ────────────────────────────────────

            if settings_actions.close {
                self.panel_mode = PanelMode::ControlBar;
            }

            // ── handle control bar actions ─────────────────────────────────

            if actions.play_pause {
                self.control_bar_state.is_playing = !self.control_bar_state.is_playing;
                if let Some(dec) = &self.video_decoder {
                    dec.paused.store(!self.control_bar_state.is_playing, Ordering::Relaxed);
                }
            }

            if actions.cycle_speed {
                self.control_bar_state.speed_index =
                    (self.control_bar_state.speed_index + 1) % SPEEDS.len();
                if let Some(dec) = &self.video_decoder {
                    dec.speed_index.store(
                        self.control_bar_state.speed_index as u32,
                        Ordering::Relaxed,
                    );
                }
            }

            if actions.cycle_loop {
                let current_us = self
                    .video_decoder
                    .as_ref()
                    .map(|d| d.current_pts_us.load(Ordering::Relaxed))
                    .unwrap_or(0);

                match self.control_bar_state.loop_state {
                    0 => {
                        // First click — set loop start.
                        if let Some(dec) = &self.video_decoder {
                            dec.loop_start_us.store(current_us, Ordering::Relaxed);
                            dec.loop_state.store(1, Ordering::Relaxed);
                        }
                        self.control_bar_state.loop_state = 1;
                    }
                    1 => {
                        // Second click — set loop end and activate.
                        if let Some(dec) = &self.video_decoder {
                            dec.loop_end_us.store(current_us, Ordering::Relaxed);
                            dec.loop_state.store(2, Ordering::Relaxed);
                        }
                        self.control_bar_state.loop_state = 2;
                    }
                    _ => {
                        // Third click — clear loop.
                        if let Some(dec) = &self.video_decoder {
                            dec.loop_state.store(0, Ordering::Relaxed);
                        }
                        self.control_bar_state.loop_state = 0;
                    }
                }
            }

            if let Some(frac) = actions.seek_frac {
                if let Some(dec) = &self.video_decoder {
                    if dec.duration_us > 0 {
                        let target_us = (frac as f64 * dec.duration_us as f64) as u64;
                        *dec.seek_request.lock().unwrap() = Some(target_us);
                    }
                }
            }

            if actions.show_settings && self.panel_mode != PanelMode::Settings {
                // Lazily create the settings panel the first time.
                if self.settings_panel.is_none() {
                    let target_fmt = vr.swapchain_format;
                    // 800×500 px canvas displayed at 1.6 m × 1.0 m, centred at eye level.
                    self.settings_panel = Some(PanelRenderer::new(
                        &renderer.device,
                        target_fmt,
                        800,
                        500,
                        glam::Vec3::new(0.0, 1.2, -2.0),
                        1.6,
                        1.0,
                    ));
                }
                self.panel_mode = PanelMode::Settings;
            }

            if actions.show_browser && self.panel_mode != PanelMode::Browser {
                // Lazily create browser state and panel the first time.
                if self.browser_state.is_none() {
                    let start_dir = self.video_path
                        .as_deref()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                    let target_fmt = vr.swapchain_format;

                    self.browser_state = Some(BrowserState::new(
                        start_dir,
                        self.video_path.clone(),
                    ));
                    // 800×600 px canvas displayed at 1.6 m × 1.2 m, centred at eye level.
                    self.browser_panel = Some(PanelRenderer::new(
                        &renderer.device,
                        target_fmt,
                        800,
                        600,
                        glam::Vec3::new(0.0, 1.2, -2.0),
                        1.6,
                        1.2,
                    ));
                }
                self.panel_mode = PanelMode::Browser;
            }

            // ── B / Y button: toggle panel visibility ─────────────────────
            if actions.menu_toggle {
                if self.panel_mode != PanelMode::Hidden {
                    self.hidden_from = self.panel_mode;
                    self.panel_mode  = PanelMode::Hidden;
                } else {
                    self.panel_mode = self.hidden_from;
                }
            }

            if actions.exit {
                vr.request_exit();
            }
        }

        renderer.window().request_redraw();
    }
}
