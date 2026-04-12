/// A mounted volume (drive letter, network share, or filesystem mount point)
/// that the file browser can navigate to.
#[derive(Debug, Clone)]
pub struct Volume {
    /// The root path of the volume (e.g. `C:\`, `/Volumes/MyDrive`, `/mnt/usb`).
    pub root: std::path::PathBuf,
    /// Short human-readable label shown in the UI (e.g. `C:`, `MyDrive`, `usb`).
    pub label: String,
}

/// Return the list of currently available volumes.
pub fn list_volumes() -> Vec<Volume> {
    imp::list_volumes()
}

/// Return the volume root that contains `path`.
///
/// Falls back to the filesystem root (or drive root on Windows) if no
/// specific volume match is found.
pub fn volume_root_of(path: &std::path::Path) -> std::path::PathBuf {
    imp::volume_root_of(path)
}

// ── Windows ────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod imp {
    use std::path::PathBuf;
    use super::Volume;

    pub fn list_volumes() -> Vec<Volume> {
        // Probe each possible drive letter.  `PathBuf::is_dir()` returns false
        // quickly for drive letters with no root directory (e.g. unmounted optical
        // drives) so this is safe to call for all 26 letters.
        (b'A'..=b'Z')
            .filter_map(|letter| {
                let root = PathBuf::from(format!("{}:\\", letter as char));
                if root.is_dir() {
                    Some(Volume {
                        label: format!("{}:", letter as char),
                        root,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn volume_root_of(path: &std::path::Path) -> PathBuf {
        use std::path::Component;
        for component in path.components() {
            if let Component::Prefix(p) = component {
                use std::path::Prefix::*;
                match p.kind() {
                    Disk(b) | VerbatimDisk(b) => {
                        return PathBuf::from(format!("{}:\\", b as char));
                    }
                    _ => {}
                }
            }
        }
        // Fallback: try to find the existing root directory.
        PathBuf::from("C:\\")
    }
}

// ── macOS ──────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod imp {
    use std::path::{Component, PathBuf};
    use super::Volume;

    pub fn list_volumes() -> Vec<Volume> {
        let mut volumes = Vec::new();

        // /Volumes contains all mounted volumes, including the system disk.
        let base = PathBuf::from("/Volumes");
        if let Ok(rd) = std::fs::read_dir(&base) {
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    let label = entry.file_name().to_string_lossy().into_owned();
                    volumes.push(Volume { root: path, label });
                }
            }
        }

        // If /Volumes is empty or missing, fall back to root.
        if volumes.is_empty() {
            volumes.push(Volume {
                root: PathBuf::from("/"),
                label: "/".to_string(),
            });
        }

        volumes
    }

    pub fn volume_root_of(path: &std::path::Path) -> PathBuf {
        let comps: Vec<_> = path.components().take(3).collect();
        if comps.len() >= 3 {
            if let (Component::RootDir, Component::Normal(d), Component::Normal(name)) =
                (&comps[0], &comps[1], &comps[2])
            {
                if d.eq_ignore_ascii_case("Volumes") {
                    let mut root = PathBuf::from("/Volumes");
                    root.push(name);
                    return root;
                }
            }
        }
        PathBuf::from("/")
    }
}

// ── Linux / other Unix ─────────────────────────────────────────────────────

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    use std::path::PathBuf;
    use super::Volume;

    pub fn list_volumes() -> Vec<Volume> {
        let mut volumes = vec![Volume {
            root:  PathBuf::from("/"),
            label: "/".to_string(),
        }];

        // /mnt/* — direct children are volume roots.
        collect_dir("/mnt", 1, &mut volumes);

        // /media/* or /media/USER/* — one or two levels of nesting.
        if let Ok(rd) = std::fs::read_dir("/media") {
            for entry in rd.flatten() {
                let path = entry.path();
                if !path.is_dir() { continue; }
                let name = entry.file_name().to_string_lossy().into_owned();
                // Check whether this looks like a volume name or a user name.
                // A quick heuristic: if it has children that are directories,
                // treat it as a user directory and descend one level.
                let children_are_vols = std::fs::read_dir(&path)
                    .ok()
                    .map_or(false, |mut rd| rd.any(|e| e.map_or(false, |e| e.path().is_dir())));
                if children_are_vols {
                    // /media/<user>/<vol>
                    collect_dir(path.to_str().unwrap_or("/media"), 1, &mut volumes);
                } else {
                    volumes.push(Volume { root: path, label: name });
                }
            }
        }

        volumes
    }

    fn collect_dir(base: &str, _levels: usize, out: &mut Vec<Volume>) {
        if let Ok(rd) = std::fs::read_dir(base) {
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    let label = entry.file_name().to_string_lossy().into_owned();
                    out.push(Volume { root: path, label });
                }
            }
        }
    }

    pub fn volume_root_of(path: &std::path::Path) -> PathBuf {
        let s = path.to_string_lossy();

        if let Some(rest) = s.strip_prefix("/mnt/") {
            let name = rest.split('/').next().unwrap_or("");
            if !name.is_empty() {
                return PathBuf::from(format!("/mnt/{name}"));
            }
        }

        if let Some(rest) = s.strip_prefix("/media/") {
            let parts: Vec<&str> = rest.splitn(3, '/').collect();
            if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
                return PathBuf::from(format!("/media/{}/{}", parts[0], parts[1]));
            }
            if parts.len() >= 1 && !parts[0].is_empty() {
                return PathBuf::from(format!("/media/{}", parts[0]));
            }
        }

        PathBuf::from("/")
    }
}
