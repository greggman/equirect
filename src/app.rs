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
use crate::ui::browser::{BrowserEntry, BrowserState, Location};
use crate::ui::settings::VideoSettings;
use crate::video_layer::{VideoSwapchain, use_xr_layer};
use crate::video_meta;

// ── helpers ───────────────────────────────────────────────────────────────────

fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Short display name for a video source string.
fn source_display_name(s: &str) -> String {
    if is_url(s) {
        let raw = s.trim_end_matches('/').rsplit('/').next().unwrap_or(s);
        crate::net::url_decode(raw)
    } else {
        std::path::Path::new(s)
            .file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_string()
    }
}

fn source_to_location(s: &str) -> Location {
    if is_url(s) {
        Location::Remote(s.to_string())
    } else {
        Location::Local(PathBuf::from(s))
    }
}

/// Returns the source string of the video `delta` steps away from `current`
/// in a sorted folder listing.  Uses the folder cache; falls back to a fresh
/// read (blocking for remote) and caches the result.
fn adjacent_video(
    current: &str,
    folder_cache: &mut std::collections::HashMap<String, Vec<BrowserEntry>>,
    delta: i32,
) -> Option<String> {
    use crate::ui::browser::{load_local_entries, remote_entries};

    let parent_loc = if is_url(current) {
        Location::Remote(crate::net::parent_url(current))
    } else {
        let p = std::path::Path::new(current);
        Location::Local(p.parent()?.to_path_buf())
    };

    let key = cache_key(&parent_loc);

    let entries: Vec<BrowserEntry> = if let Some(cached) = folder_cache.get(&key) {
        cached.clone()
    } else {
        let fresh = match &parent_loc {
            Location::Local(dir) => load_local_entries(dir),
            Location::Remote(url) => {
                match crate::net::list_http_dir(url) {
                    Ok(items) => remote_entries(items),
                    Err(e) => { eprintln!("prev/next: listing {url} failed: {e}"); return None; }
                }
            }
        };
        folder_cache.insert(key, fresh.clone());
        fresh
    };

    let videos: Vec<String> = entries.iter().filter_map(|e| {
        if let BrowserEntry::Video(_, loc) = e {
            Some(location_to_source(loc.clone()))
        } else {
            None
        }
    }).collect();

    if videos.is_empty() { return None; }
    let n   = videos.len() as i32;
    let pos = videos.iter().position(|v: &String| {
        v.trim_end_matches('/') == current.trim_end_matches('/')
    })? as i32;
    Some(videos[((pos + delta).rem_euclid(n)) as usize].clone())
}

fn cache_key(loc: &Location) -> String {
    match loc {
        Location::Local(p)  => p.to_string_lossy().into_owned(),
        Location::Remote(u) => u.trim_end_matches('/').to_string(),
    }
}

fn location_to_source(loc: Location) -> String {
    match loc {
        Location::Local(p)  => p.to_string_lossy().into_owned(),
        Location::Remote(u) => u,
    }
}

fn load_meta(source: &str) -> Option<video_meta::VideoMeta> {
    if is_url(source) {
        video_meta::load_url(source)
    } else {
        video_meta::load(std::path::Path::new(source))
    }
}

fn save_meta(source: &str, meta: &video_meta::VideoMeta) {
    if is_url(source) {
        video_meta::save_url(source, meta);
    } else {
        video_meta::save(std::path::Path::new(source), meta);
    }
}


// ── App ───────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum PanelMode {
    ControlBar,
    Browser,
    Settings,
    Hidden,
}
use crate::local::LocalMode;
use crate::mode::Mode;
use crate::ui::assets::UiAssets;
use crate::ui::control_bar::{ControlBarState, SPEEDS};
use crate::ui::panel::PanelRenderer;
use crate::video::decoder::VideoDecoder;
use crate::video::texture::VideoTexture;
use crate::video_renderer::VideoRenderer;
use crate::vr::{VrContext, VrPreInit};

pub struct App {
    // Drop order matters: mode before renderer so any XR session is destroyed
    // while the wgpu Vulkan device is still alive.
    mode: Option<Box<dyn Mode>>,
    /// True if `--local` was passed on the command line — skip VR setup entirely.
    local_mode: bool,
    /// Shared GPU UI assets (font atlas, icon sheet, pipeline).
    ui_assets: Option<UiAssets>,
    renderer: Option<Renderer>,
    video_decoder: Option<VideoDecoder>,
    video_texture: Option<VideoTexture>,
    video_renderer: Option<VideoRenderer>,    // fisheye shader fallback
    video_swapchain: Option<VideoSwapchain>,  // XR layer path (all other modes)
    panel_renderer: Option<PanelRenderer>,
    pointer_renderer: Option<PointerRenderer>,
    control_bar_state: ControlBarState,
    /// Current video source: a local file path or an http(s):// URL.
    video_source: Option<String>,
    // File browser
    browser_state: Option<BrowserState>,
    browser_panel: Option<PanelRenderer>,
    /// Cached folder contents keyed by normalized location string.
    /// Persists across browser open/close cycles.  Entries are invalidated
    /// when the user explicitly clicks a folder to enter it.
    folder_cache: std::collections::HashMap<String, Vec<BrowserEntry>>,
    /// Directory to show in the browser at startup; consumed on first use.
    initial_browser_dir: Option<PathBuf>,
    // Settings
    video_settings: VideoSettings,
    settings_panel: Option<PanelRenderer>,
    // Panel visibility
    panel_mode: PanelMode,
    seek_target_secs: Option<f64>,
    seek_timeout: Option<std::time::Instant>,
    /// IPC receiver: messages from a second-instance launch (file path or URL).
    ipc_rx: std::sync::mpsc::Receiver<String>,
    /// Pending IPC-requested video source, queued until VR is ready.
    pending_ipc_source: Option<String>,
    /// If set, seek to this position (seconds) once the first video opens.
    initial_start_secs: Option<f64>,
}


impl App {
    pub fn new(
        video_source: Option<String>,
        initial_browser_dir: Option<PathBuf>,
        ipc_rx: std::sync::mpsc::Receiver<String>,
        initial_start_secs: Option<f64>,
        local_mode: bool,
    ) -> Self {
        let panel_mode = if video_source.is_none() && initial_browser_dir.is_some() {
            PanelMode::Browser
        } else {
            PanelMode::ControlBar
        };
        Self {
            mode: None,
            local_mode,
            ui_assets: None,
            renderer: None,
            video_decoder: None,
            video_texture: None,
            video_renderer: None,
            video_swapchain: None,
            panel_renderer: None,
            pointer_renderer: None,
            control_bar_state: ControlBarState::default(),
            video_source,
            browser_state: None,
            browser_panel: None,
            folder_cache: std::collections::HashMap::new(),
            initial_browser_dir,
            video_settings: VideoSettings::new(),
            settings_panel: None,
            panel_mode,
            seek_target_secs: None,
            seek_timeout: None,
            ipc_rx,
            pending_ipc_source: None,
            initial_start_secs,
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
                .create_window(
                    Window::default_attributes()
                        .with_title(if cfg!(debug_assertions) {
                            concat!("equirect v", env!("CARGO_PKG_VERSION"), "-dev")
                        } else {
                            concat!("equirect v", env!("CARGO_PKG_VERSION"))
                        })
                        .with_inner_size(winit::dpi::LogicalSize::new(640u32, 428u32)),
                )
                .expect("Failed to create window"),
        );

        // Set title-bar (ICON_SMALL) and taskbar (ICON_BIG) icons by loading
        // the icon embedded in the PE by build.rs/winresource (resource ID 1).
        // SHChangeNotify is required to flush the shell's per-path icon cache;
        // without it the taskbar shows a stale blank icon for the exe path.
        #[cfg(target_os = "windows")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
            use windows::Win32::System::LibraryLoader::GetModuleHandleW;
            use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_UPDATEITEM, SHCNF_PATHW, SHCNF_FLUSH};
            use windows::Win32::UI::WindowsAndMessaging::{
                LoadIconW, SendMessageW, SetClassLongPtrW,
                WM_SETICON, ICON_BIG, ICON_SMALL, GCLP_HICON, GCLP_HICONSM,
            };
            use windows::core::PCWSTR;
            if let Ok(exe) = std::env::current_exe() {
                let wide: Vec<u16> = exe.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                unsafe { SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW | SHCNF_FLUSH, Some(wide.as_ptr() as *const _), None); }
            }
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    unsafe {
                        let hwnd  = HWND(h.hwnd.get() as *mut core::ffi::c_void);
                        let hmod  = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
                        let hinst = Some(HINSTANCE(hmod.0));
                        if let Ok(icon) = LoadIconW(hinst, PCWSTR(1 as *const u16)) {
                            let lparam = Some(LPARAM(icon.0 as isize));
                            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG   as usize)), lparam);
                            SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), lparam);
                            SetClassLongPtrW(hwnd, GCLP_HICON,   icon.0 as isize);
                            SetClassLongPtrW(hwnd, GCLP_HICONSM, icon.0 as isize);
                        }
                    }
                }
            }
        }

        // VR setup is skipped entirely in --local mode so the app can run
        // without an OpenXR runtime or headset.
        let vr_pre = if self.local_mode { None } else { VrPreInit::new() };
        let xr_exts = vr_pre.as_ref().map(|v| v.required_device_extensions()).unwrap_or_default();

        let mut renderer = Renderer::new(window, &xr_exts);

        let mode: Option<Box<dyn Mode>> = if self.local_mode {
            Some(Box::new(LocalMode::new(&renderer)))
        } else {
            let vr = vr_pre.and_then(|pre| VrContext::new(&renderer, pre));
            if let Some(ref vr) = vr {
                renderer.prepare_for_xr(vr.swapchain_format);
            }
            vr.map(|v| Box::new(v) as Box<dyn Mode>)
        };

        let target_fmt = mode
            .as_ref()
            .map(|m| m.target_format())
            .unwrap_or_else(|| renderer.surface_format());

        if let Some(ref source) = self.video_source {
            if let Some(meta) = load_meta(source) {
                self.video_settings = meta.settings;
            }
            match VideoDecoder::open(source.clone()) {
                Err(e) => {
                    self.control_bar_state.error = Some(format!("Can't play video: {e}"));
                    self.panel_mode = PanelMode::ControlBar;
                }
                Ok(decoder) => {
                    self.control_bar_state.error = None;
                    let tex = decoder.gpu_bgra_handle
                        .and_then(|h| VideoTexture::new_gpu(
                            &renderer.device, &renderer.instance, &renderer.adapter,
                            decoder.width, decoder.height, h,
                        ))
                        .unwrap_or_else(|| VideoTexture::new(
                            &renderer.device, decoder.width, decoder.height, decoder.is_nv12,
                        ));
                    let vr_rend = VideoRenderer::new(
                        &renderer.device, target_fmt,
                        decoder.width, decoder.height,
                    );
                    let sc = mode.as_ref().and_then(|m| {
                        m.create_video_swapchain(&renderer.device, decoder.width, decoder.height)
                    });
                    self.control_bar_state.video_name = source_display_name(source);
                    if decoder.duration_us > 0 {
                        self.control_bar_state.duration_secs =
                            decoder.duration_us as f64 / 1_000_000.0;
                    }
                    // --start / -t: seek to the requested position on first open.
                    if let Some(secs) = self.initial_start_secs.take() {
                        let target_us = (secs * 1_000_000.0) as u64;
                        *decoder.seek_request.lock().unwrap() = Some(target_us);
                        self.control_bar_state.current_secs = secs;
                        self.seek_target_secs = Some(secs);
                        self.seek_timeout = Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(30),
                        );
                    }
                    self.video_decoder   = Some(decoder);
                    self.video_texture   = Some(tex);
                    self.video_renderer  = Some(vr_rend);
                    self.video_swapchain = sc;
                }
            }
        }

        let cb_pos      = mode.as_ref().map(|m| m.control_bar_position()).unwrap_or(glam::Vec3::new(0.0, 0.0, -2.0));
        let browser_pos = mode.as_ref().map(|m| m.browser_position()).unwrap_or(glam::Vec3::new(0.0, 1.2, -2.0));

        // Size the control bar texture to fit its content exactly — no slack
        // around the icon row, no empty space below the slider.  The constants
        // come from control_bar.rs so changing the icon count or row heights
        // there automatically resizes the panel.
        use crate::ui::control_bar::{CONTROL_BAR_CONTENT_H, CONTROL_BAR_CONTENT_W};
        use crate::ui::ui::PADDING as UI_PADDING;
        let cb_pixel_w = (CONTROL_BAR_CONTENT_W + UI_PADDING * 2.0).ceil() as u32;
        let cb_pixel_h = (CONTROL_BAR_CONTENT_H + UI_PADDING * 2.0).ceil() as u32;
        let cb_world_w = 1.6_f32;
        let cb_world_h = cb_world_w * cb_pixel_h as f32 / cb_pixel_w as f32;

        self.panel_renderer = Some(PanelRenderer::new(
            &renderer.device, target_fmt,
            cb_pixel_w, cb_pixel_h,
            cb_pos,
            cb_world_w, cb_world_h,
        ));

        self.pointer_renderer = Some(PointerRenderer::new(&renderer.device, target_fmt));

        if let Some(dir) = self.initial_browser_dir.take() {
            let current_loc = self.video_source.as_deref().map(source_to_location);
            let loc = Location::Local(dir);
            let cached = self.folder_cache.get(&cache_key(&loc)).cloned();
            self.browser_state = Some(BrowserState::new(loc, current_loc, cached));
            self.browser_panel = Some(PanelRenderer::new(
                &renderer.device, target_fmt,
                800, 600,
                browser_pos,
                1.6, 1.2,
            ));
        }

        self.ui_assets = Some(UiAssets::new(&renderer.device, &renderer.queue));
        self.renderer = Some(renderer);
        self.mode = mode;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Forward every event to the active mode (LocalMode uses these for
        // mouse/keyboard input; VrContext ignores them).
        if let Some(m) = &mut self.mode {
            m.handle_window_event(&event);
        }
        match event {
            WindowEvent::CloseRequested => { event_loop.exit(); }
            WindowEvent::Resized(size) => {
                if let Some(r) = &mut self.renderer { r.resize(size); }
            }
            WindowEvent::RedrawRequested => {
                let Some(r) = &mut self.renderer else { return };
                // In modes that render directly to the window (LocalMode), the
                // surface is drawn from `about_to_wait` — don't double-render
                // the logo on top.
                let renders_to_window = self.mode.as_ref().map_or(false, |m| m.renders_to_window());
                if !renders_to_window {
                    if !r.render() {
                        let size = r.window().inner_size();
                        r.resize(size);
                    }
                }
                r.window().request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = &self.renderer else { return };

        // Poll IPC channel for messages from a second instance.
        while let Ok(msg) = self.ipc_rx.try_recv() {
            if msg.is_empty() {
                renderer.window().focus_window();
            } else {
                self.pending_ipc_source = Some(msg);
            }
        }

        if self.control_bar_state.duration_secs == 0.0 {
            if let Some(dec) = &self.video_decoder {
                if dec.duration_us > 0 {
                    self.control_bar_state.duration_secs =
                        dec.duration_us as f64 / 1_000_000.0;
                }
            }
        }

        if let (Some(decoder), Some(texture)) = (&self.video_decoder, &self.video_texture) {
            if let Some(frame) = decoder.take_frame() {
                texture.upload(&renderer.queue, &frame);
                if let Some(vr_rend) = &mut self.video_renderer {
                    vr_rend.set_texture(&renderer.device, texture);
                }
                if let Some(sc) = &mut self.video_swapchain {
                    sc.set_texture(&renderer.device, &renderer.queue, texture);
                }
            }
            let pts_secs = decoder.current_pts_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
            if let Some(target) = self.seek_target_secs {
                let timed_out = self.seek_timeout
                    .map_or(false, |t| std::time::Instant::now() >= t);
                if pts_secs >= target - 0.1 || timed_out {
                    self.control_bar_state.current_secs = pts_secs;
                    self.seek_target_secs = None;
                    self.seek_timeout = None;
                }
            } else {
                self.control_bar_state.current_secs = pts_secs;
            }
        }

        if let Some(mode) = &mut self.mode {
            if mode.exit_requested() {
                event_loop.exit();
                return;
            }
            if mode.should_quit() {
                renderer
                    .device
                    .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
                    .ok();
                self.video_swapchain = None;
                self.mode = None;
                return;
            }

            let use_layer = mode.supports_video_layer()
                && use_xr_layer(&self.video_settings, mode.supports_equirect2());
            let video_settings_snap = self.video_settings.clone();
            let video_layer_arg =
                if use_layer { self.video_swapchain.as_mut().map(|sc| (sc, &video_settings_snap)) }
                else { None };
            let video_shader_arg = if !use_layer {
                self.video_renderer.as_ref()
                    .zip(self.video_texture.as_ref())
                    .map(|(r, t)| (r, t, &video_settings_snap))
            } else { None };

            let panel_arg =
                if self.panel_mode == PanelMode::ControlBar {
                    self.panel_renderer.as_mut().map(|p| (p, &self.control_bar_state))
                } else { None };
            let browser_arg =
                if self.panel_mode == PanelMode::Browser {
                    self.browser_panel.as_mut().zip(self.browser_state.as_mut())
                } else { None };
            let settings_arg =
                if self.panel_mode == PanelMode::Settings {
                    self.settings_panel.as_mut().map(|p| (p, &mut self.video_settings))
                } else { None };
            let pointer_arg =
                if self.panel_mode != PanelMode::Hidden { self.pointer_renderer.as_ref() }
                else { None };

            let ui_assets = self.ui_assets.as_mut().expect("UiAssets not initialised");
            let (actions, browser_actions, settings_actions) = mode.render_frame(
                renderer,
                ui_assets,
                video_layer_arg,
                video_shader_arg,
                panel_arg,
                browser_arg,
                settings_arg,
                pointer_arg,
            );

            // ── browser actions ───────────────────────────────────────────────

            if browser_actions.close {
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::ControlBar;
            }

            // ".." / parent navigation — use cache.
            if let Some(loc) = browser_actions.navigate {
                if let Some(dir) = loc.as_local() {
                    let vol_root = crate::volumes::volume_root_of(dir);
                    video_meta::save_last_dir(dir);
                    video_meta::save_volume_last_dir(&vol_root, dir);
                }
                let cached = self.folder_cache.get(&cache_key(&loc)).cloned();
                if let Some(bs) = &mut self.browser_state {
                    bs.navigate_to(loc, cached);
                }
            }

            // Explicit folder click — invalidate that folder's cache entry.
            if let Some(loc) = browser_actions.select_dir {
                if let Some(dir) = loc.as_local() {
                    let vol_root = crate::volumes::volume_root_of(dir);
                    video_meta::save_last_dir(dir);
                    video_meta::save_volume_last_dir(&vol_root, dir);
                }
                self.folder_cache.remove(&cache_key(&loc));
                if let Some(bs) = &mut self.browser_state {
                    bs.navigate_to(loc, None);
                }
            }

            if let Some(vol_root) = browser_actions.select_volume {
                if let Some(bs) = &self.browser_state {
                    if let Some(cur_dir) = bs.location.as_local() {
                        let cur_root = crate::volumes::volume_root_of(cur_dir);
                        video_meta::save_volume_last_dir(&cur_root, cur_dir);
                    }
                }
                let target = video_meta::resolve_dir_for_volume(&vol_root);
                video_meta::save_last_dir(&target);
                let loc = Location::Local(target);
                let cached = self.folder_cache.get(&cache_key(&loc)).cloned();
                if let Some(bs) = &mut self.browser_state {
                    bs.navigate_to(loc, cached);
                }
            }

            // Write freshly-loaded entries into the cache.
            if let Some(bs) = &mut self.browser_state {
                if bs.just_loaded {
                    self.folder_cache.insert(cache_key(&bs.location), bs.entries.clone());
                    bs.just_loaded = false;
                }
            }

            // Determine whether we're loading a new source from the browser or from IPC.
            let play_source: Option<String> = if let Some(play_loc) = browser_actions.play {
                if let Location::Local(ref p) = play_loc {
                    if let Some(parent) = p.parent() {
                        video_meta::save_last_dir(parent);
                    }
                }
                self.browser_state = None;
                self.browser_panel = None;
                self.panel_mode    = PanelMode::Hidden;
                Some(location_to_source(play_loc))
            } else if let Some(src) = self.pending_ipc_source.take() {
                renderer.window().focus_window();
                // Directory → open browser there; file/URL → play as video.
                if std::path::Path::new(&src).is_dir() {
                    let dir = std::path::PathBuf::from(&src);
                    video_meta::save_last_dir(&dir);
                    let current_loc = self.video_source.as_deref().map(source_to_location);
                    let loc = Location::Local(dir);
                    let cached = self.folder_cache.get(&cache_key(&loc)).cloned();
                    self.browser_state = Some(BrowserState::new(loc, current_loc, cached));
                    if self.browser_panel.is_none() {
                        self.browser_panel = Some(PanelRenderer::new(
                            &renderer.device, mode.target_format(),
                            800, 600,
                            mode.browser_position(),
                            1.6, 1.2,
                        ));
                    }
                    self.panel_mode = PanelMode::Browser;
                    None
                } else {
                    let loc = source_to_location(&src);
                    if let Location::Local(ref p) = loc {
                        if let Some(parent) = p.parent() {
                            video_meta::save_last_dir(parent);
                        }
                    }
                    self.browser_state = None;
                    self.browser_panel = None;
                    self.panel_mode    = PanelMode::Hidden;
                    Some(src)
                }
            } else {
                None
            };

            if let Some(new_source) = play_source {
                let target_fmt = mode.target_format();

                self.video_settings = load_meta(&new_source)
                    .map(|m| m.settings)
                    .unwrap_or_else(|| {
                        let inherited = self.video_settings.clone();
                        save_meta(&new_source, &video_meta::VideoMeta { settings: inherited.clone() });
                        inherited
                    });

                self.video_swapchain = None;
                self.video_decoder   = None;
                self.video_texture   = None;
                self.video_renderer  = None;

                match VideoDecoder::open(new_source.clone()) {
                    Err(e) => {
                        self.control_bar_state.error = Some(format!("Can't play video: {e}"));
                        self.panel_mode  = PanelMode::ControlBar;
                        self.video_source = Some(new_source);
                    }
                    Ok(decoder) => {
                        self.control_bar_state.error = None;
                        let tex = decoder.gpu_bgra_handle
                            .and_then(|h| VideoTexture::new_gpu(
                                &renderer.device, &renderer.instance, &renderer.adapter,
                                decoder.width, decoder.height, h,
                            ))
                            .unwrap_or_else(|| VideoTexture::new(
                                &renderer.device, decoder.width, decoder.height, decoder.is_nv12,
                            ));
                        let vr_rend = VideoRenderer::new(
                            &renderer.device, target_fmt,
                            decoder.width, decoder.height,
                        );
                        let sc = mode.create_video_swapchain(
                            &renderer.device, decoder.width, decoder.height,
                        );
                        self.control_bar_state.video_name = source_display_name(&new_source);
                        self.control_bar_state.duration_secs =
                            if decoder.duration_us > 0 { decoder.duration_us as f64 / 1_000_000.0 }
                            else { 0.0 };
                        self.control_bar_state.current_secs = 0.0;
                        self.control_bar_state.is_playing   = true;
                        self.video_source    = Some(new_source);
                        self.video_decoder   = Some(decoder);
                        self.video_texture   = Some(tex);
                        self.video_renderer  = Some(vr_rend);
                        self.video_swapchain = sc;
                    }
                }
            }

            // ── settings actions ──────────────────────────────────────────────

            if settings_actions.changed {
                if let Some(ref source) = self.video_source {
                    save_meta(source, &video_meta::VideoMeta {
                        settings: self.video_settings.clone(),
                    });
                }
            }
            if settings_actions.close {
                self.panel_mode = PanelMode::ControlBar;
            }

            // ── control bar actions ───────────────────────────────────────────

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
                        self.control_bar_state.speed_index as u32, Ordering::Relaxed,
                    );
                }
            }

            if actions.cycle_loop {
                let current_us = self.video_decoder.as_ref()
                    .map(|d| d.current_pts_us.load(Ordering::Relaxed))
                    .unwrap_or(0);
                match self.control_bar_state.loop_state {
                    0 => {
                        if let Some(dec) = &self.video_decoder {
                            dec.loop_start_us.store(current_us, Ordering::Relaxed);
                            dec.loop_state.store(1, Ordering::Relaxed);
                        }
                        self.control_bar_state.loop_state = 1;
                    }
                    1 => {
                        if let Some(dec) = &self.video_decoder {
                            dec.loop_end_us.store(current_us, Ordering::Relaxed);
                            dec.loop_state.store(2, Ordering::Relaxed);
                        }
                        self.control_bar_state.loop_state = 2;
                    }
                    _ => {
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
                        dec.audio_seek.store(target_us, Ordering::Relaxed);
                        dec.audio_flush_gen.fetch_add(1, Ordering::Relaxed);
                        let target_secs = target_us as f64 / 1_000_000.0;
                        self.control_bar_state.current_secs = target_secs;
                        self.seek_target_secs = Some(target_secs);
                        self.seek_timeout = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(500),
                        );
                    }
                }
            }

            if let Some(delta_secs) = actions.seek_delta_secs {
                if let Some(dec) = &self.video_decoder {
                    if dec.duration_us > 0 {
                        let duration_secs = dec.duration_us as f64 / 1_000_000.0;
                        let target_secs =
                            (self.control_bar_state.current_secs + delta_secs)
                            .rem_euclid(duration_secs);
                        let target_us = (target_secs * 1_000_000.0) as u64;
                        *dec.seek_request.lock().unwrap() = Some(target_us);
                        dec.audio_seek.store(target_us, Ordering::Relaxed);
                        dec.audio_flush_gen.fetch_add(1, Ordering::Relaxed);
                        self.control_bar_state.current_secs = target_secs;
                        self.seek_target_secs = Some(target_secs);
                        self.seek_timeout = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(500),
                        );
                    }
                }
            }

            // ── prev / next video ─────────────────────────────────────────────
            let nav_delta: Option<i32> = if actions.prev { Some(-1) }
                else if actions.next { Some(1) }
                else { None };

            if let Some(delta) = nav_delta {
                let next = self.video_source.as_deref()
                    .and_then(|s| adjacent_video(s, &mut self.folder_cache, delta));

                if let Some(new_source) = next {
                    let target_fmt = mode.target_format();
                    self.video_settings = load_meta(&new_source)
                        .map(|m| m.settings)
                        .unwrap_or_else(|| {
                            let inherited = self.video_settings.clone();
                            save_meta(&new_source, &video_meta::VideoMeta { settings: inherited.clone() });
                            inherited
                        });
                    self.video_swapchain = None;
                    self.video_decoder   = None;
                    self.video_texture   = None;
                    self.video_renderer  = None;
                    match VideoDecoder::open(new_source.clone()) {
                        Err(e) => {
                            self.control_bar_state.error = Some(format!("Can't play video: {e}"));
                            self.panel_mode  = PanelMode::ControlBar;
                            self.video_source = Some(new_source);
                        }
                        Ok(decoder) => {
                            self.control_bar_state.error = None;
                            let tex = decoder.gpu_bgra_handle
                                .and_then(|h| VideoTexture::new_gpu(
                                    &renderer.device, &renderer.instance, &renderer.adapter,
                                    decoder.width, decoder.height, h,
                                ))
                                .unwrap_or_else(|| VideoTexture::new(
                                    &renderer.device, decoder.width, decoder.height, decoder.is_nv12,
                                ));
                            let vr_rend = VideoRenderer::new(
                                &renderer.device, target_fmt,
                                decoder.width, decoder.height,
                            );
                            let sc = mode.create_video_swapchain(
                                &renderer.device, decoder.width, decoder.height,
                            );
                            self.control_bar_state.video_name = source_display_name(&new_source);
                            self.control_bar_state.duration_secs =
                                if decoder.duration_us > 0 { decoder.duration_us as f64 / 1_000_000.0 }
                                else { 0.0 };
                            self.control_bar_state.current_secs = 0.0;
                            self.control_bar_state.is_playing   = true;
                            self.video_source    = Some(new_source);
                            self.video_decoder   = Some(decoder);
                            self.video_texture   = Some(tex);
                            self.video_renderer  = Some(vr_rend);
                            self.video_swapchain = sc;
                        }
                    }
                }
            }

            if actions.show_settings && self.panel_mode != PanelMode::Settings {
                if self.settings_panel.is_none() {
                    self.settings_panel = Some(PanelRenderer::new(
                        &renderer.device, mode.target_format(),
                        800, 600,
                        mode.settings_position(),
                        1.6, 1.0,
                    ));
                }
                self.panel_mode = PanelMode::Settings;
            }

            if actions.show_browser && self.panel_mode != PanelMode::Browser {
                if self.browser_state.is_none() {
                    let start_loc = match self.video_source.as_deref() {
                        Some(s) if is_url(s) => {
                            Location::Remote(crate::net::parent_url(s))
                        }
                        Some(s) => {
                            let p = std::path::Path::new(s);
                            Location::Local(
                                p.parent().map(|d| d.to_path_buf())
                                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
                            )
                        }
                        None => Location::Local(std::env::current_dir().unwrap_or_default()),
                    };
                    let current_loc = self.video_source.as_deref().map(source_to_location);
                    let cached = self.folder_cache.get(&cache_key(&start_loc)).cloned();
                    self.browser_state = Some(BrowserState::new(start_loc, current_loc, cached));
                    self.browser_panel = Some(PanelRenderer::new(
                        &renderer.device, mode.target_format(),
                        800, 600,
                        mode.browser_position(),
                        1.6, 1.2,
                    ));
                }
                self.panel_mode = PanelMode::Browser;
            }

            if actions.menu_toggle {
                self.panel_mode = match self.panel_mode {
                    PanelMode::Hidden     => PanelMode::ControlBar,
                    PanelMode::ControlBar => PanelMode::Hidden,
                    PanelMode::Settings   => PanelMode::ControlBar,
                    PanelMode::Browser    => PanelMode::ControlBar,
                };
            }

            if actions.exit {
                mode.request_exit();
                event_loop.exit();
            }
        }

        renderer.window().request_redraw();
    }
}
