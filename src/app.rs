use std::sync::Arc;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::renderer::Renderer;

#[derive(Default)]
pub struct App {
    renderer: Option<Renderer>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() {
            let window = Arc::new(
                event_loop
                    .create_window(Window::default_attributes().with_title("vrust-v"))
                    .expect("Failed to create window"),
            );
            self.renderer = Some(Renderer::new(window));
        }
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
}
