//! HTTP/HTTPS directory listing.
//!
//! Tries WebDAV PROPFIND first; falls back to scraping `<a href>` links from
//! a plain HTML page.

use anyhow::{anyhow, bail, Result};

use crate::ui::browser::VIDEO_EXTS;

// ── Public types ──────────────────────────────────────────────────────────────

pub struct RemoteItem {
    pub name:   String,
    pub url:    String,
    pub is_dir: bool,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// List the contents of a remote HTTP/HTTPS "directory".
///
/// 1. Tries WebDAV `PROPFIND` with `Depth: 1`.
/// 2. On failure falls back to `GET` + HTML anchor scraping.
///
/// Returns dirs first (sorted), then videos (sorted).  Non-video files are
/// omitted.
pub fn list_http_dir(url: &str) -> Result<Vec<RemoteItem>> {
    // Normalise: ensure trailing slash so relative hrefs resolve correctly.
    let base = ensure_trailing_slash(url);

    match propfind(&base) {
        Ok(items) => return Ok(items),
        Err(e)    => crate::vprintln!("net: PROPFIND failed ({e}), trying HTML scrape"),
    }

    html_listing(&base)
}

/// Derive the "parent directory" URL by stripping the last path segment.
/// Returns the same URL if already at the server root.
pub fn parent_url(url: &str) -> String {
    let s = url.trim_end_matches('/');
    // Find the last '/' after the scheme+host ("https://host/...")
    let scheme_end = s.find("://").map(|p| p + 3).unwrap_or(0);
    if let Some(slash) = s[scheme_end..].rfind('/') {
        let cut = scheme_end + slash;
        if cut > scheme_end {
            return format!("{}/", &s[..cut]);
        }
    }
    // Already at root — return with trailing slash
    format!("{}/", s)
}

// ── PROPFIND ──────────────────────────────────────────────────────────────────

const PROPFIND_BODY: &str =
    r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/><D:displayname/></D:prop></D:propfind>"#;

fn propfind(url: &str) -> Result<Vec<RemoteItem>> {
    let resp = ureq::request("PROPFIND", url)
        .set("Depth", "1")
        .set("Content-Type", "application/xml; charset=utf-8")
        .send_string(PROPFIND_BODY)
        .map_err(|e| anyhow!("PROPFIND: {e}"))?;

    if resp.status() != 207 {
        bail!("PROPFIND returned HTTP {}", resp.status());
    }

    let body = resp.into_string().map_err(|e| anyhow!("PROPFIND body: {e}"))?;
    parse_propfind(&body, url)
}

/// Parse a WebDAV `207 Multi-Status` XML body.
///
/// Uses simple byte-level scanning rather than a full XML parser so we stay
/// dependency-light and remain tolerant of namespace-prefix variations (e.g.
/// `<D:href>` vs `<ns0:href>` vs `<href>`).
fn parse_propfind(xml: &str, base_url: &str) -> Result<Vec<RemoteItem>> {
    let xml_lo = xml.to_ascii_lowercase();
    let mut items: Vec<RemoteItem> = Vec::new();
    let mut pos = 0;

    loop {
        // Find the start of a <*:response> (or <response>) open tag.
        let resp_open = match find_tag_open(&xml_lo, "response", pos) {
            Some(p) => p,
            None    => break,
        };
        // Find the matching </*:response> close tag.
        let resp_close = match find_tag_close(&xml_lo, "response", resp_open + 1) {
            Some(p) => p,
            None    => break,
        };

        let block    = &xml   [resp_open..resp_close];
        let block_lo = &xml_lo[resp_open..resp_close];

        if let Some(href) = extract_tag_text(block, block_lo, "href") {
            let href = href.trim();
            // A block contains <*:collection/> if this entry is a directory.
            let is_dir = block_lo.contains(":collection") || block_lo.contains("<collection");
            let item_url = resolve_url(base_url, href);

            // Skip the directory listing itself.
            if !same_url(base_url, &item_url) {
                let name = display_name(&item_url);
                if is_dir || has_video_ext(&name) {
                    items.push(RemoteItem { name, url: item_url, is_dir });
                }
            }
        }

        pos = resp_close;
    }

    sort_items(&mut items);
    Ok(items)
}

// ── HTML scraping ─────────────────────────────────────────────────────────────

fn html_listing(url: &str) -> Result<Vec<RemoteItem>> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| anyhow!("GET {url}: {e}"))?;
    let body = resp.into_string().map_err(|e| anyhow!("GET body: {e}"))?;
    Ok(scrape_html_links(&body, url))
}

fn scrape_html_links(html: &str, base_url: &str) -> Vec<RemoteItem> {
    let mut items: Vec<RemoteItem> = Vec::new();
    let html_lo = html.to_ascii_lowercase();
    let mut search = 0;

    while let Some(rel) = html_lo[search..].find("href=") {
        let at = search + rel + 5; // position just after "href="
        let rest = &html[at..];

        let (raw_href, advance) = extract_attr_value(rest);
        search = at + advance;

        if raw_href.starts_with('#')
            || raw_href.starts_with("javascript:")
            || raw_href.is_empty()
        {
            continue;
        }

        let item_url = resolve_url(base_url, raw_href);
        if same_url(base_url, &item_url) {
            continue;
        }

        let is_dir = raw_href.ends_with('/');
        let name   = display_name(&item_url);
        if name.is_empty() { continue; }

        if is_dir || has_video_ext(&name) {
            items.push(RemoteItem { name, url: item_url, is_dir });
        }
    }

    // Deduplicate by URL, then sort.
    items.sort_by(|a, b| a.url.cmp(&b.url));
    items.dedup_by(|a, b| a.url == b.url);
    sort_items(&mut items);
    items
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn ensure_trailing_slash(url: &str) -> String {
    if url.ends_with('/') { url.to_string() } else { format!("{url}/") }
}

/// Find the byte position of `<prefix:name>` or `<name>` inside `haystack`
/// (already lowercased), starting at `from`.
fn find_tag_open(hay: &str, name: &str, from: usize) -> Option<usize> {
    let mut pos = from;
    loop {
        let lt = hay[pos..].find('<').map(|p| p + pos)?;
        let after_lt = lt + 1;
        // Skip optional namespace prefix.
        let tag_start = hay[after_lt..]
            .find(':')
            .filter(|&p| p < 32) // sanity: prefix is short
            .map(|p| after_lt + p + 1)
            .unwrap_or(after_lt);
        if hay[tag_start..].starts_with(name) {
            let after_name = tag_start + name.len();
            let ch = hay[after_name..].chars().next().unwrap_or(' ');
            if ch == '>' || ch == ' ' || ch == '/' {
                return Some(lt);
            }
        }
        pos = after_lt;
    }
}

/// Find the byte position *after* the closing `</prefix:name>` tag, starting
/// at `from` (already lowercased).
fn find_tag_close(hay: &str, name: &str, from: usize) -> Option<usize> {
    let mut pos = from;
    loop {
        let lt = hay[pos..].find("</").map(|p| p + pos)?;
        let after_lt = lt + 2;
        let tag_start = hay[after_lt..]
            .find(':')
            .filter(|&p| p < 32)
            .map(|p| after_lt + p + 1)
            .unwrap_or(after_lt);
        if hay[tag_start..].starts_with(name) {
            let after_name = tag_start + name.len();
            let ch = hay[after_name..].chars().next().unwrap_or(' ');
            if ch == '>' || ch == ' ' {
                // Return position after the closing '>'
                let end = hay[after_name..].find('>').map(|p| after_name + p + 1)?;
                return Some(end);
            }
        }
        pos = after_lt;
    }
}

/// Extract the text content between `<*:tag>` and `</*:tag>`.
fn extract_tag_text<'a>(src: &'a str, src_lo: &str, tag: &str) -> Option<&'a str> {
    let open_end = find_tag_open(src_lo, tag, 0)
        .and_then(|p| src_lo[p..].find('>').map(|q| p + q + 1))?;
    let close_start = find_tag_open(src_lo, &format!("/{tag}"), open_end)?;
    Some(&src[open_end..close_start])
}

/// Resolve `href` against `base_url`.
fn resolve_url(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if href.starts_with('/') {
        // Absolute-path reference — prepend scheme + host.
        let scheme_end = base.find("://").map(|p| p + 3).unwrap_or(0);
        let host_end   = base[scheme_end..].find('/').map(|p| p + scheme_end)
            .unwrap_or(base.len());
        return format!("{}{}", &base[..host_end], href);
    }
    // Relative reference — append to base directory.
    let base_dir = if base.ends_with('/') {
        base
    } else {
        base.rfind('/').map(|p| &base[..p + 1]).unwrap_or(base)
    };
    format!("{}{}", base_dir, href)
}

/// True when two URLs refer to the same resource (ignoring trailing slash).
fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// Human-readable name for a URL: last path segment, URL-decoded.
fn display_name(url: &str) -> String {
    let s = url.trim_end_matches('/');
    let raw = s.rsplit('/').next().unwrap_or(s);
    url_decode(raw)
}

fn has_video_ext(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("");
    VIDEO_EXTS.iter().any(|&v| v.eq_ignore_ascii_case(ext))
}

fn sort_items(items: &mut Vec<RemoteItem>) {
    items.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true,  false) => std::cmp::Ordering::Less,
        (false, true)  => std::cmp::Ordering::Greater,
        _              => a.name.cmp(&b.name),
    });
}

/// Extract the value of an HTML attribute that immediately follows `href=`.
/// Returns `(value, bytes_consumed)`.
fn extract_attr_value(s: &str) -> (&str, usize) {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'"') {
        let end = s[1..].find('"').unwrap_or(s.len() - 1);
        (&s[1..1 + end], end + 2)
    } else if bytes.first() == Some(&b'\'') {
        let end = s[1..].find('\'').unwrap_or(s.len() - 1);
        (&s[1..1 + end], end + 2)
    } else {
        let end = s
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .unwrap_or(s.len());
        (&s[..end], end)
    }
}

/// Decode `%XX` percent-encoding in a URL component.
pub fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                // Only decode ASCII printable range to avoid mojibake.
                let decoded = (h << 4) | l;
                if decoded >= 0x20 && decoded < 0x7f {
                    out.push(decoded as char);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _           => None,
    }
}
