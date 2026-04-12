mod app;
mod audio;
mod input;
mod pointer_renderer;
mod renderer;
mod ui;
mod video;
mod video_layer;
mod video_meta;
mod video_renderer;
mod vr;

fn main() {
    let arg = std::env::args().nth(1).map(std::path::PathBuf::from);

    let (video_path, initial_browser_dir) = match arg {
        // Explicit directory → open browser there.
        Some(p) if p.is_dir() => (None, Some(p)),
        // Explicit file → play it, and save its folder as the last browsed dir.
        Some(p) => {
            if let Some(parent) = p.parent() {
                if parent.is_dir() {
                    video_meta::save_last_dir(parent);
                }
            }
            (Some(p), None)
        }
        // Nothing → restore last dir, fall back to video dir, then home.
        None => {
            use directories::UserDirs;
            let dir = video_meta::load_last_dir()
                .or_else(|| {
                    UserDirs::new().and_then(|u| {
                        u.video_dir().map(|d| d.to_path_buf())
                    })
                })
                .or_else(|| {
                    directories::UserDirs::new()
                        .map(|u| u.home_dir().to_path_buf())
                });
            (None, dir)
        }
    };

    let event_loop = winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut app = app::App::new(video_path, initial_browser_dir);
    event_loop.run_app(&mut app).expect("Event loop failed");
}
