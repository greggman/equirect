mod app;
mod audio;
mod input;
mod logo;
mod net;
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

/// Named-pipe path used for single-instance IPC.
/// Fixed name → no port conflicts; not network-accessible → no remote control.
const PIPE_NAME: &str = r"\\.\pipe\com.greggman.equirect";

/// Single-instance guard using a Windows named pipe.
///
/// - If a server pipe already exists (another instance is running), opens it as
///   a client, writes `arg`, and returns `None` — the caller should exit.
/// - Otherwise creates the server pipe, spawns a listener thread, and returns
///   `Some(rx)`.  Messages arriving on the pipe are forwarded to `rx`.
fn single_instance(arg: Option<&str>) -> Option<std::sync::mpsc::Receiver<String>> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_SHARE_NONE,
        OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
        PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::core::PCWSTR;

    let pipe_wide: Vec<u16> = PIPE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let pipe_pcwstr = PCWSTR(pipe_wide.as_ptr());

    // ── Try to become the server ──────────────────────────────────────────────
    // FILE_FLAG_FIRST_PIPE_INSTANCE causes CreateNamedPipeW to fail if the name
    // already exists, giving us a definitive "another instance is running" signal.
    let server = unsafe {
        CreateNamedPipeW(
            pipe_pcwstr,
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            0,    // outbound buffer (unused for inbound-only)
            4096, // inbound buffer
            0,    // default client timeout
            None,
        )
    };

    if server != INVALID_HANDLE_VALUE {
        // We are the first instance — spawn the listener thread.
        // HANDLE is not Send; transmit as usize and reconstruct inside the thread.
        let server_raw = server.0 as usize;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let server = windows::Win32::Foundation::HANDLE(server_raw as *mut core::ffi::c_void);
            let mut buf = vec![0u8; 4096];
            loop {
                // Block until a client connects.  ConnectNamedPipe returns an error
                // with ERROR_PIPE_CONNECTED when the client connected before this
                // call — that is still a successful connection.
                let result = unsafe { ConnectNamedPipe(server, None) };
                let connected = result.is_ok()
                    || result
                        .err()
                        .map_or(false, |e| e.code() == ERROR_PIPE_CONNECTED.to_hresult());
                if !connected {
                    unsafe { let _ = DisconnectNamedPipe(server); }
                    continue;
                }

                // Read newline-delimited messages from this client.
                let mut acc = String::new();
                loop {
                    let mut read = 0u32;
                    let ok = unsafe {
                        ReadFile(server, Some(&mut buf), Some(&mut read), None).is_ok()
                    };
                    if !ok || read == 0 {
                        break;
                    }
                    if let Ok(s) = std::str::from_utf8(&buf[..read as usize]) {
                        acc.push_str(s);
                        while let Some(pos) = acc.find('\n') {
                            let line = acc[..pos].to_string();
                            acc.drain(..=pos);
                            let _ = tx.send(line);
                        }
                    }
                }

                unsafe { let _ = DisconnectNamedPipe(server); }
            }
        });
        return Some(rx);
    }

    // ── Another instance is running: connect as a client ─────────────────────
    // WaitNamedPipeW blocks until the server has called ConnectNamedPipe (i.e.,
    // it is actually ready to accept a connection).  This handles the race where
    // the server thread hasn't reached ConnectNamedPipe yet.
    if !unsafe { WaitNamedPipeW(pipe_pcwstr, 5000).as_bool() } {
        // Timed out or failed — run as a standalone instance.
        let (_tx, rx) = std::sync::mpsc::channel::<String>();
        return Some(rx);
    }

    let client = unsafe {
        CreateFileW(
            pipe_pcwstr,
            GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };

    match client {
        Ok(handle) => {
            let msg = format!("{}\n", arg.unwrap_or(""));
            unsafe {
                let _ = WriteFile(handle, Some(msg.as_bytes()), None, None);
                let _ = CloseHandle(handle);
            }
            None // Handed off to existing instance — caller should exit.
        }
        Err(_) => {
            // Connection failed despite WaitNamedPipeW — run standalone.
            let (_tx, rx) = std::sync::mpsc::channel::<String>();
            Some(rx)
        }
    }
}

fn main() {
    // Parse args: strip -v/--verbose flags; first remaining arg is a path or URL.
    let mut verbose = false;
    let mut first_arg: Option<String> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-v" | "--verbose" => verbose = true,
            _ if first_arg.is_none() => first_arg = Some(arg),
            _ => {}
        }
    }
    VERBOSE.store(verbose, std::sync::atomic::Ordering::Relaxed);

    // Single-instance check: hand off to existing process if one is running.
    let ipc_rx = match single_instance(first_arg.as_deref()) {
        Some(rx) => rx,
        None     => return,  // Handed off to existing instance; exit.
    };

    let (video_source, initial_browser_dir) = match first_arg {
        // http / https URL → stream directly via MF
        Some(s) if s.starts_with("http://") || s.starts_with("https://") => {
            (Some(s), None)
        }

        // Explicit directory → open browser there
        Some(s) => {
            let p = std::path::PathBuf::from(&s);
            if p.is_dir() {
                (None, Some(p))
            } else {
                // Explicit file
                if let Some(parent) = p.parent() {
                    if parent.is_dir() {
                        video_meta::save_last_dir(parent);
                    }
                }
                (Some(s), None)
            }
        }

        // Nothing → restore last dir, fall back to video dir, then home
        None => {
            use directories::UserDirs;
            let dir = video_meta::load_last_dir()
                .or_else(|| {
                    UserDirs::new().and_then(|u| u.video_dir().map(|d| d.to_path_buf()))
                })
                .or_else(|| {
                    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
                });
            (None, dir)
        }
    };

    let event_loop = winit::event_loop::EventLoop::new().expect("Failed to create event loop");
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut app = app::App::new(video_source, initial_browser_dir, ipc_rx);
    event_loop.run_app(&mut app).expect("Event loop failed");
}
