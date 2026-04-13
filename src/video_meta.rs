use std::collections::HashMap;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use sha2::{Digest, Sha256};

use crate::ui::settings::VideoSettings;

/// Per-video metadata stored in the user config directory.
/// Saved as `video-<sha256-of-absolute-path>.json`.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct VideoMeta {
    pub settings: VideoSettings,
}

fn config_meta_path(key: &str) -> Option<std::path::PathBuf> {
    let dirs = ProjectDirs::from("", "", "equirect")?;
    let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
    let mut p = dirs.config_dir().to_path_buf();
    p.push(format!("video-{}.json", hash));
    Some(p)
}

fn meta_path(video_path: &Path) -> Option<std::path::PathBuf> {
    let canonical = video_path.canonicalize().ok()
        .unwrap_or_else(|| video_path.to_path_buf());
    config_meta_path(&canonical.to_string_lossy())
}

/// Load saved metadata for `video_path`.  Returns `None` if no metadata exists yet.
pub fn load(video_path: &Path) -> Option<VideoMeta> {
    let path = meta_path(video_path)?;
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save metadata for `video_path`.  Silently ignores I/O errors.
pub fn save(video_path: &Path, meta: &VideoMeta) {
    let Some(path) = meta_path(video_path) else { return };
    write_meta(&path, meta);
}

/// Load saved metadata for a URL.  Returns `None` if no metadata exists yet.
pub fn load_url(url: &str) -> Option<VideoMeta> {
    let path = config_meta_path(url)?;
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save metadata for a URL.  Silently ignores I/O errors.
pub fn save_url(url: &str, meta: &VideoMeta) {
    let Some(path) = config_meta_path(url) else { return };
    write_meta(&path, meta);
}

fn write_meta(path: &std::path::Path, meta: &VideoMeta) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let _ = std::fs::write(path, json);
    }
}

// ── app-wide state (state.json) ────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct AppState {
    last_dir: Option<PathBuf>,
    /// Maps a volume root path (as a string) to the last browsed directory
    /// within that volume.  Missing on old state files; defaults to empty.
    #[serde(default)]
    volume_last_dirs: HashMap<String, PathBuf>,
}

fn config_dir() -> Option<PathBuf> {
    Some(ProjectDirs::from("", "", "equirect")?.config_dir().to_path_buf())
}

fn state_path() -> Option<PathBuf> {
    let mut p = config_dir()?;
    p.push("state.json");
    Some(p)
}

fn load_app_state() -> AppState {
    let Some(path) = state_path() else { return AppState::default() };
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_app_state(state: &AppState) {
    let Some(path) = state_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(&path, json);
    }
}

/// Returns the last browsed directory if it was saved and still exists.
pub fn load_last_dir() -> Option<PathBuf> {
    let dir = load_app_state().last_dir?;
    if dir.is_dir() { Some(dir) } else { None }
}

/// Persist `dir` as the last browsed directory.
pub fn save_last_dir(dir: &Path) {
    let mut state = load_app_state();
    state.last_dir = Some(dir.to_path_buf());
    save_app_state(&state);
}

// ── per-volume last-directory ──────────────────────────────────────────────

/// Persist `dir` as the last browsed directory for the given `volume_root`.
pub fn save_volume_last_dir(volume_root: &Path, dir: &Path) {
    let mut state = load_app_state();
    let key = volume_root.to_string_lossy().into_owned();
    state.volume_last_dirs.insert(key, dir.to_path_buf());
    save_app_state(&state);
}

/// Return the best directory to navigate to when the user selects `volume_root`.
///
/// Loads the last saved directory for that volume, then walks up the path
/// until an existing directory is found.  Falls back to `volume_root` itself.
pub fn resolve_dir_for_volume(volume_root: &Path) -> PathBuf {
    let key = volume_root.to_string_lossy().into_owned();
    let saved = load_app_state().volume_last_dirs.get(&key).cloned();

    if let Some(mut dir) = saved {
        loop {
            if dir.is_dir() {
                return dir;
            }
            // Don't climb above the volume root.
            if dir == volume_root {
                break;
            }
            match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            }
        }
    }

    volume_root.to_path_buf()
}
