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
use crate::ui::browser::{BrowserState, VIDEO_EXTS};
use crate::ui::settings::VideoSettings;
use crate::video_layer::{VideoSwapchain, use_xr_layer};
use crate::video_meta;

/// Returns the path of the video that is `delta` steps away from `current`
/// in a sorted list of video files in the same directory, wrapping around.
/// Returns `None` if the directory can't be read or there are no other videos.
fn adjacent_video(current: &std::path::Path, delta: i32) -> Option<PathBuf> {
    let dir = current.parent()?;
    let mut videos: Vec<PathBuf> = std::fs::read_dir(dir).ok()?
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str())?;
            if VIDEO_EXTS.iter().any(|&v| v.eq_ignore_ascii_case(ext)) {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    if videos.is_empty() { return None; }
    videos.sort_by(|a, b| {
        a.file_name().cmp(&b.file_name())
    });
    let n = videos.len() as i32;
    let pos = videos.iter().position(|p| p == current)? as i32;
    let next_pos = ((pos + delta).rem_euclid(n)) as usize;
    Some(videos[next_pos].clone())
}

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
    video_renderer: Option<VideoRenderer>,    // fisheye shader fallback
    video_swapchain: Option<VideoSwapchain>,  // XR layer path (all other modes)
    panel_renderer: Option<PanelRenderer>,
    pointer_renderer: Option<PointerRenderer>,
    control_bar_state: ControlBarState,
    video_path: Option<PathBuf>,
    // File browser
    browser_state: Option<BrowserState>,
    browser_panel: Option<PanelRenderer>,
    /// Directory to show in the browser at startup; consumed on first use.
    initial_browser_dir: Option<PathBuf>,
    // Settings
    video_settings: VideoSettings,
    settings_panel: Option<PanelRenderer>,
    // Panel visibility
    panel_mode: PanelMode,
}

impl App {
    pub fn new(video_path: Option<PathBuf>, initial_browser_dir: Option<PathBuf>) -> Self {
        // If we have an initial browser dir but no video, start with the browser open.
        let panel_mode = if video_path.is_none() && initial_browser_dir.is_some() {
            PanelMode::Browser
        } else {
            PanelMode::ControlBar
        };
        Self {
            vr: None,
            renderer: None,
            video_decoder: None,
            video_texture: None,
            video_renderer: None,
            video_swapchain: None,
            panel_renderer: None,
            pointer_renderer: None,
            control_bar_state: ControlBarState::default(),
            video_path,
            browser_state: None,
            browser_panel: None,
            initial_browser_dir,
            video_settings: VideoSettings::new(),
            settings_panel: None,
            panel_mode,
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
            // Restore per-video settings before opening.
            if let Some(meta) = video_meta::load(path) {
                self.video_settings = meta.settings;
            }
            match VideoDecoder::open(path.clone()) {
                Err(e) => eprintln!("Failed to open video: {e}"),
                Ok(decoder) => {
                    let tex = VideoTexture::new(&renderer.device, decoder.width, decoder.height);
                    // VideoRenderer is only used for the fisheye shader fallback.
                    let vr_rend = VideoRenderer::new(
                        &renderer.device,
                        target_fmt,
                        decoder.width,
                        decoder.height,
                    );
                    // VideoSwapchain for the XR composition-layer path.
                    let sc = vr.as_ref().and_then(|vr| {
                        vr.create_video_swapchain(&renderer.device, decoder.width, decoder.height)
                    });

                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        self.control_bar_state.video_name = stem.to_owned();
                    }
                    if decoder.duration_us > 0 {
                        self.control_bar_state.duration_secs =
                            decoder.duration_us as f64 / 1_000_000.0;
                    }

                    self.video_decoder   = Some(decoder);
                    self.video_texture   = Some(tex);
                    self.video_renderer  = Some(vr_rend);
                    self.video_swapchain = sc;
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

        // If an initial browser directory was provided, create the browser panel now.
        if let Some(dir) = self.initial_browser_dir.take() {
            self.browser_state = Some(BrowserState::new(dir, self.video_path.clone()));
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
        if let (Some(decoder), Some(texture)) = (&self.video_decoder, &self.video_texture) {
            if let Some(frame) = decoder.take_frame() {
                texture.upload(&renderer.queue, &frame.data);
                // Rebind texture in both the shader renderer and the swapchain blit.
                if let Some(vr_rend) = &mut self.video_renderer {
                    vr_rend.set_texture(&renderer.device, texture);
                }
                if let Some(sc) = &mut self.video_swapchain {
                    sc.set_texture(&renderer.device, texture);
                }
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
                // Drop VideoSwapchain BEFORE VrContext so xrDestroySwapchain
                // is called while the session is still alive.
                self.video_swapchain = None;
                self.vr = None;
                return;
            }

            // Route video based on current projection setting.
            let use_layer = use_xr_layer(&self.video_settings, vr.has_equirect2);
            // Clone settings so we can hold &mut self.video_settings for the settings panel
            // at the same time as we pass &VideoSettings to the video layer.
            let video_settings_snap = self.video_settings.clone();
            let video_layer_arg: Option<(&mut crate::video_layer::VideoSwapchain, &VideoSettings)> =
                if use_layer {
                    self.video_swapchain.as_mut().map(|sc| (sc, &video_settings_snap))
                } else {
                    None
                };
            let video_shader_arg = if !use_layer {
                self.video_renderer.as_ref()
                    .zip(self.video_texture.as_ref())
                    .map(|(r, t)| (r, t, &video_settings_snap))
            } else {
                None
            };

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

            // Pointers are shown whenever a panel is visible; hidden otherwise.
            let pointer_arg = if self.panel_mode != PanelMode::Hidden {
                self.pointer_renderer.as_ref()
            } else {
                None
            };

            let (actions, browser_actions, settings_actions) = vr.render_frame(
                renderer,
                video_layer_arg,
                video_shader_arg,
                panel_arg,
                browser_arg,
                settings_arg,
                pointer_arg,
            );

            // ── handle browser actions ─────────────────────────────────────

            if browser_actions.close {
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::ControlBar;
            }

            if let Some(dir) = browser_actions.navigate {
                video_meta::save_last_dir(&dir);
                if let Some(bs) = &mut self.browser_state {
                    bs.navigate_to(dir);
                }
            }

            if let Some(new_path) = browser_actions.play {
                // Save the folder this video lives in as the last browsed dir.
                if let Some(parent) = new_path.parent() {
                    video_meta::save_last_dir(parent);
                }
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::Hidden;

                let target_fmt = vr.swapchain_format;

                // Restore saved settings for the new video; if none exist, inherit
                // the current settings and immediately save them for the new file.
                self.video_settings = video_meta::load(&new_path)
                    .map(|m| m.settings)
                    .unwrap_or_else(|| {
                        let inherited = self.video_settings.clone();
                        video_meta::save(&new_path, &video_meta::VideoMeta {
                            settings: inherited.clone(),
                        });
                        inherited
                    });

                // Drop old video components before opening the new file.
                self.video_swapchain = None;
                self.video_decoder   = None;
                self.video_texture   = None;
                self.video_renderer  = None;

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
                        let sc = vr.create_video_swapchain(
                            &renderer.device, decoder.width, decoder.height,
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
                        self.video_path      = Some(new_path);
                        self.video_decoder   = Some(decoder);
                        self.video_texture   = Some(tex);
                        self.video_renderer  = Some(vr_rend);
                        self.video_swapchain = sc;
                    }
                }
            }

            // ── handle settings actions ────────────────────────────────────

            if settings_actions.changed {
                if let Some(ref path) = self.video_path {
                    video_meta::save(path, &video_meta::VideoMeta {
                        settings: self.video_settings.clone(),
                    });
                }
            }

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
                        // Signal the audio thread and flush stale buffered audio.
                        dec.audio_seek.store(target_us, Ordering::Relaxed);
                        dec.audio_flush_gen.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            // ── prev / next video ──────────────────────────────────────────
            let nav_delta: Option<i32> = if actions.prev { Some(-1) }
                else if actions.next { Some(1) }
                else { None };

            if let Some(delta) = nav_delta {
                if let Some(new_path) = self.video_path.as_deref()
                    .and_then(|p| adjacent_video(p, delta))
                {
                    let target_fmt = vr.swapchain_format;

                    self.video_settings = video_meta::load(&new_path)
                        .map(|m| m.settings)
                        .unwrap_or_else(|| {
                            let inherited = self.video_settings.clone();
                            video_meta::save(&new_path, &video_meta::VideoMeta {
                                settings: inherited.clone(),
                            });
                            inherited
                        });

                    self.video_swapchain = None;
                    self.video_decoder   = None;
                    self.video_texture   = None;
                    self.video_renderer  = None;

                    match crate::video::decoder::VideoDecoder::open(new_path.clone()) {
                        Err(e) => eprintln!("Failed to open video: {e}"),
                        Ok(decoder) => {
                            let tex = crate::video::texture::VideoTexture::new(
                                &renderer.device, decoder.width, decoder.height,
                            );
                            let vr_rend = crate::video_renderer::VideoRenderer::new(
                                &renderer.device, target_fmt,
                                decoder.width, decoder.height,
                            );
                            let sc = vr.create_video_swapchain(
                                &renderer.device, decoder.width, decoder.height,
                            );
                            if let Some(stem) = new_path.file_stem().and_then(|s| s.to_str()) {
                                self.control_bar_state.video_name = stem.to_owned();
                            }
                            self.control_bar_state.duration_secs =
                                if decoder.duration_us > 0 {
                                    decoder.duration_us as f64 / 1_000_000.0
                                } else { 0.0 };
                            self.control_bar_state.current_secs = 0.0;
                            self.control_bar_state.is_playing   = true;
                            self.video_path      = Some(new_path);
                            self.video_decoder   = Some(decoder);
                            self.video_texture   = Some(tex);
                            self.video_renderer  = Some(vr_rend);
                            self.video_swapchain = sc;
                        }
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

            // ── B / Y button: back button ──────────────────────────────────
            // Hidden      → show control bar (and pointers)
            // ControlBar  → hide control bar (and pointers)
            // Settings    → back to control bar
            // Browser     → back to control bar
            if actions.menu_toggle {
                self.panel_mode = match self.panel_mode {
                    PanelMode::Hidden     => PanelMode::ControlBar,
                    PanelMode::ControlBar => PanelMode::Hidden,
                    PanelMode::Settings   => PanelMode::ControlBar,
                    PanelMode::Browser    => PanelMode::ControlBar,
                };
            }

            if actions.exit {
                vr.request_exit();
            }
        }

        renderer.window().request_redraw();
    }
}
