mod app;
mod renderer;
mod ui;
mod video;
mod video_renderer;
mod vr;

fn main() {
    let video_path = std::env::args().nth(1).map(std::path::PathBuf::from);

    let event_loop = winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut app = app::App::new(video_path);
    event_loop.run_app(&mut app).expect("Event loop failed");
}
