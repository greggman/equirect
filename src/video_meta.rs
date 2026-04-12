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

fn meta_path(video_path: &Path) -> Option<std::path::PathBuf> {
    let dirs = ProjectDirs::from("", "", "vrust-v")?;
    let canonical = video_path.canonicalize().ok()
        .unwrap_or_else(|| video_path.to_path_buf());
    let hash = format!("{:x}", Sha256::digest(canonical.to_string_lossy().as_bytes()));
    let mut p = dirs.config_dir().to_path_buf();
    p.push(format!("video-{}.json", hash));
    Some(p)
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
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(meta) {
        let _ = std::fs::write(&path, json);
    }
}

// ── app-wide state (state.json) ────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct AppState {
    last_dir: Option<PathBuf>,
}

fn config_dir() -> Option<PathBuf> {
    Some(ProjectDirs::from("", "", "vrust-v")?.config_dir().to_path_buf())
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
    save_app_state(&AppState { last_dir: Some(dir.to_path_buf()) });
}
