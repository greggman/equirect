use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::renderer::Renderer;
use crate::vr::{VrContext, VrPreInit};

const AUTO_EXIT_SECS: u64 = 5;

pub struct App {
    // Drop order matters: vr must be dropped before renderer so the XR session
    // is destroyed while the wgpu Vulkan device is still alive.
    vr: Option<VrContext>,
    renderer: Option<Renderer>,
    start: Instant,
    /// True once the auto-exit timer has fired and we've asked XR to shut down.
    exit_requested: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            vr: None,
            renderer: None,
            start: Instant::now(),
            exit_requested: false,
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

        // Pre-init XR to query required Vulkan device extensions before device creation.
        let vr_pre = VrPreInit::new();
        let xr_exts = vr_pre.as_ref()
            .map(|v| v.required_device_extensions())
            .unwrap_or_default();

        let mut renderer = Renderer::new(window, &xr_exts);

        // Complete XR initialisation using the now-properly-configured device.
        let vr = vr_pre.and_then(|pre| VrContext::new(&renderer, pre));
        if let Some(ref vr) = vr {
            renderer.prepare_for_xr(vr.swapchain_format);
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

    /// Called every iteration in Poll mode — drive the XR frame loop here.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // When the timer fires, ask the XR runtime for a clean shutdown instead
        // of hard-exiting. The runtime will send STOPPING → EXITING, at which
        // point should_quit becomes true and we drop VrContext cleanly.
        if !self.exit_requested && self.start.elapsed() >= Duration::from_secs(AUTO_EXIT_SECS) {
            self.exit_requested = true;
            if let Some(vr) = &self.vr {
                vr.request_exit();
            } else {
                println!("Auto-exit after {AUTO_EXIT_SECS}s");
                event_loop.exit();
                return;
            }
        }

        let Some(renderer) = &self.renderer else { return };

        if let Some(vr) = &mut self.vr {
            if vr.should_quit {
                // Flush all in-flight GPU work before the Oculus runtime cleans up
                // Vulkan resources inside xrDestroySession / xrDestroyInstance.
                renderer.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).ok();
                self.vr = None;
                if self.exit_requested {
                    println!("Auto-exit after {AUTO_EXIT_SECS}s");
                    event_loop.exit();
                }
                return;
            }
            vr.render_frame(renderer);
        }

        renderer.window().request_redraw();
    }
}
