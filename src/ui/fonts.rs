/// Register system CJK fonts as egui fallbacks.
///
/// egui's built-in font covers Latin, Greek, Cyrillic, and a few other scripts.
/// For CJK (Japanese, Chinese, Korean) and other non-Latin scripts we probe a
/// short list of well-known Windows font paths and load whichever ones exist.
/// Users who have filenames in a given script will have the matching language
/// pack — and therefore the matching font — installed.
///
/// Each found font is appended to the `Proportional` family's fallback list, so
/// Latin text continues to use the built-in font and CJK characters fall through
/// to these.
pub fn install_system_fonts(ctx: &egui::Context) {
    // Ordered by preference within each script group.  Only paths that exist
    // on the system are loaded; missing ones are silently skipped.
    #[cfg(target_os = "windows")]
    let candidates: &[&str] = &[
        // Japanese
        r"C:\Windows\Fonts\meiryo.ttc",     // Meiryo — clean, modern
        r"C:\Windows\Fonts\msgothic.ttc",   // MS Gothic — always present with JP pack
        // Chinese Simplified
        r"C:\Windows\Fonts\msyh.ttc",       // Microsoft YaHei — clean, modern
        r"C:\Windows\Fonts\simsun.ttc",     // SimSun — always present with SC pack
        // Chinese Traditional
        r"C:\Windows\Fonts\msjh.ttc",       // Microsoft JhengHei
        r"C:\Windows\Fonts\mingliu.ttc",    // MingLiU — always present with TC pack
        // Korean
        r"C:\Windows\Fonts\malgun.ttf",     // Malgun Gothic — clean, modern
        r"C:\Windows\Fonts\gulim.ttc",      // Gulim — always present with KO pack
    ];

    #[cfg(not(target_os = "windows"))]
    let candidates: &[&str] = &[];

    if candidates.is_empty() {
        return;
    }

    let mut fonts = egui::FontDefinitions::default();

    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else { continue };

        // Use the filename stem as a unique key (e.g. "meiryo", "msgothic").
        let name = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("sys_font")
            .to_owned();

        fonts
            .font_data
            .insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));

        // Append after the built-in font so Latin still uses the default.
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push(name);
    }

    ctx.set_fonts(fonts);
}
