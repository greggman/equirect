use std::path::PathBuf;
use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::renderer::Renderer;
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

        // Open the video file and create GPU resources for it.
        if let Some(ref path) = self.video_path {
            match VideoDecoder::open(path.clone()) {
                Err(e) => eprintln!("Failed to open video: {e}"),
                Ok(decoder) => {
                    let tex = VideoTexture::new(&renderer.device, decoder.width, decoder.height);

                    // Use the XR swapchain format when available, otherwise the
                    // desktop surface format (so the pipeline target format matches).
                    let fmt = vr
                        .as_ref()
                        .map(|v| v.swapchain_format)
                        .unwrap_or_else(|| renderer.surface_format());

                    let vr_rend = VideoRenderer::new(
                        &renderer.device,
                        fmt,
                        decoder.width,
                        decoder.height,
                    );
                    self.video_decoder = Some(decoder);
                    self.video_texture = Some(tex);
                    self.video_renderer = Some(vr_rend);
                }
            }
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
                match r.render() {
                    Ok(()) => {}
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        let size = r.window().inner_size();
                        r.resize(size);
                    }
                    Err(e) => eprintln!("Render error: {e}"),
                }
                r.window().request_redraw();
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = &self.renderer else { return };

        // Upload the latest decoded frame if one is available.
        if let (Some(decoder), Some(texture), Some(vr_rend)) = (
            &self.video_decoder,
            &self.video_texture,
            &mut self.video_renderer,
        ) {
            if let Some(frame) = decoder.take_frame() {
                texture.upload(&renderer.queue, &frame.data);
                // Rebuild the bind group on the first frame (texture_bind_group is None
                // until we call set_texture at least once).
                vr_rend.set_texture(&renderer.device, texture);
            }
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
            vr.render_frame(renderer, video);
        }

        renderer.window().request_redraw();
    }
}
