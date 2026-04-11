mod app;
mod renderer;
mod vr;

fn main() {
    let event_loop = winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut app = app::App::default();
    event_loop.run_app(&mut app).expect("Event loop failed");
}
