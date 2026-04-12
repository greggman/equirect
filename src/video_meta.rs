use std::path::Path;

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
