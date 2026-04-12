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

        self.panel_renderer = Some(PanelRenderer::new(
            &renderer.device,
            target_fmt,
            800,
            160,
            glam::Vec3::new(0.0, -0.8, -2.0),
            1.0,
            0.2,
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
            let actions = vr.render_frame(
                renderer,
                video,
                self.panel_renderer.as_mut(),
                Some(&self.control_bar_state),
                self.pointer_renderer.as_ref(),
            );

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

            if actions.exit {
                vr.request_exit();
            }
        }

        renderer.window().request_redraw();
    }
}
