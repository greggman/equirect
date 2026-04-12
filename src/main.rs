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
mod volumes;
mod vr;

/// Set to `true` when `-v` / `--verbose` is passed on the command line.
pub static VERBOSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Print to stdout only when `-v` / `--verbose` was passed.
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        if crate::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            println!($($arg)*);
        }
    };
}

fn main() {
    // Parse args: strip -v/--verbose flags; first remaining arg is the path.
    let mut verbose = false;
    let mut path_arg: Option<std::path::PathBuf> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-v" | "--verbose" => verbose = true,
            _ => {
                if path_arg.is_none() {
                    path_arg = Some(std::path::PathBuf::from(arg));
                }
            }
        }
    }
    VERBOSE.store(verbose, std::sync::atomic::Ordering::Relaxed);

    let (video_path, initial_browser_dir) = match path_arg {
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
