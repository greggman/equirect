/// Return owned font bytes for the primary font and any fallbacks found on
/// the host system.  Order matters — glyphs are looked up in this sequence.
///
/// On Windows we use Segoe UI as the primary (always present on supported
/// versions) and probe a short list of CJK system fonts as fallbacks.  If
/// Segoe UI is missing for some reason, Arial is tried next.
pub fn load_font_bytes() -> (Vec<u8>, Vec<Vec<u8>>) {
    #[cfg(target_os = "windows")]
    let primary_candidates: &[&str] = &[
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\tahoma.ttf",
    ];
    #[cfg(not(target_os = "windows"))]
    let primary_candidates: &[&str] = &[];

    let primary = primary_candidates
        .iter()
        .find_map(|p| std::fs::read(p).ok())
        .unwrap_or_else(|| panic!("ui: no primary font found (looked in {primary_candidates:?})"));

    let mut fallbacks: Vec<Vec<u8>> = Vec::new();

    #[cfg(target_os = "windows")]
    let fallback_candidates: &[&str] = &[
        // Japanese
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
        // Simplified Chinese
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
        // Traditional Chinese
        r"C:\Windows\Fonts\msjh.ttc",
        r"C:\Windows\Fonts\mingliu.ttc",
        // Korean
        r"C:\Windows\Fonts\malgun.ttf",
        r"C:\Windows\Fonts\gulim.ttc",
    ];

    #[cfg(not(target_os = "windows"))]
    let fallback_candidates: &[&str] = &[];

    for path in fallback_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fallbacks.push(bytes);
        }
    }

    (primary, fallbacks)
}
