//! Embedded HTML viewer for browser visitors.
//!
//! When a directory is requested with a browser-shaped `Accept` header
//! (or via `?view=ui`), we serve a self-contained single-file SPA instead
//! of the nginx-style `<pre>` autoindex. The P2P client path is unaffected:
//! it sends `Accept: */*` and still receives the original autoindex HTML.
//!
//! Data is injected as a `<script>window.__DATA__ = ...</script>` block with
//! the literal `</` sequence escaped to `<\/` to neutralise any embedded
//! `</script>` payload from filenames, paths, or mtime strings.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::{EntryMeta, FileServerState};

pub(crate) const VIEWER_HTML: &str = include_str!("viewer.html");

/// Per-entry data shape exposed to the frontend.
#[derive(Serialize)]
struct EntryView {
    name: String,
    kind: &'static str,
    size: u64,
    mtime: String,
    mtime_ts: u64,
    ext: String,
    href: String,
}

#[derive(Serialize)]
struct ViewerData<'a> {
    path: &'a str,
    read_only: bool,
    entries: Vec<EntryView>,
}

// ---------------------------------------------------------------------------
// Public dispatch decision
// ---------------------------------------------------------------------------

/// Decide whether to serve the embedded viewer (HTML) or the legacy autoindex.
///
/// Precedence:
///   1. `?view=ui`  → viewer
///   2. `?view=raw` → autoindex (preserves the existing P2P path)
///   3. `Accept: text/html` anywhere → viewer (browsers)
///   4. anything else → autoindex (clients like reqwest default to `*/*`)
pub(crate) fn wants_viewer(headers: &axum::http::HeaderMap, query: Option<&str>) -> bool {
    if let Some(q) = query {
        for (k, v) in url_query_pairs(q) {
            if k.eq_ignore_ascii_case("view") {
                let v = v.to_ascii_lowercase();
                if v == "ui" || v == "html" {
                    return true;
                }
                if v == "raw" || v == "autoindex" {
                    return false;
                }
            }
        }
    }
    if let Some(accept) = headers.get("accept").and_then(|v| v.to_str().ok()) {
        // Browsers send a long list; reqwest sends just "*/*".
        // Per RFC 7231 §5.3.2, "Accept: text/html;q=0" explicitly rejects HTML,
        // so we honour the q=0 sentinel rather than treating presence as consent.
        for media_range in accept.split(',') {
            let mut parts = media_range.split(';');
            let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            if !mime.starts_with("text/html") {
                continue;
            }
            let mut rejected = false;
            for param in parts {
                let p = param.trim();
                if let Some(q_str) = p
                    .strip_prefix("q=")
                    .or_else(|| p.strip_prefix("Q="))
                {
                    if let Ok(q) = q_str.trim().parse::<f32>() {
                        if q == 0.0 {
                            rejected = true;
                        }
                    }
                }
            }
            if !rejected {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

pub(crate) fn render_viewer_html(
    state: &FileServerState,
    entries: &[EntryMeta],
    url_path: &str,
) -> String {
    let views: Vec<EntryView> = entries.iter().map(entry_view).collect();
    let data = ViewerData {
        path: url_path,
        read_only: state.read_only,
        entries: views,
    };
    let json = serde_json::to_string(&data)
        .expect("EntryView is serializable (String/&'static str/u64 only); \
                 this expect trips if a non-serializable field is added");
    // XSS guard: prevent any embedded </script> in the JSON from breaking out.
    let safe = json.replace("</", "<\\/");
    VIEWER_HTML.replace(
        "__DATA_PLACEHOLDER__",
        &format!("window.__DATA__ = {safe};"),
    )
}

fn entry_view(e: &EntryMeta) -> EntryView {
    let kind = classify(&e.name, e.is_dir);
    let mtime_ts = e
        .mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mtime = format_iso8601(e.mtime);
    let href = if e.is_dir {
        format!("{}/", urlencoding(&e.name))
    } else {
        urlencoding(&e.name)
    };
    EntryView {
        name: e.name.clone(),
        kind,
        size: e.size,
        mtime,
        mtime_ts,
        ext: extension(&e.name).to_string(),
        href,
    }
}

/// Classify a file by extension into one of the icon categories the UI knows.
///
/// Categories are chosen to match the design tokens (`--type-pdf`, `--type-img`,
/// etc.) and the `type-badge[data-t=...]` selectors. Unknown extensions fall
/// through to `other`.
fn classify(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = extension(name).to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "pdf",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "heic" | "avif" => "img",
        "doc" | "docx" | "rtf" | "txt" | "md" | "markdown" | "odt" | "pages" => "doc",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => "zip",
        "rs" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "c" | "cc" | "cpp" | "h" | "hpp"
        | "go" | "java" | "rb" | "sh" | "bash" | "zsh" | "html" | "css" | "scss" | "json"
        | "yaml" | "yml" | "toml" | "xml" | "sql" | "vue" | "svelte" | "lua" | "kt" | "swift" => {
            "code"
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "opus" | "wma" => "audio",
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "ogv" => "video",
        _ => "other",
    }
}

fn extension(name: &str) -> &str {
    // Treat leading-dot files (.hidden) as extensionless.
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

fn urlencoding(s: &str) -> String {
    // Minimal percent-encode for path components: keep the common safe chars.
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{:02X}", c));
        }
    }
    out
}

fn format_iso8601(t: SystemTime) -> String {
    // Pre-1970 timestamps would otherwise round-trip as "1970-01-01T00:00:00Z",
    // which is misleading. Surface the absence instead.
    let dur = match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return String::new(),
    };
    chrono::DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

fn url_query_pairs(q: &str) -> impl Iterator<Item = (String, String)> + '_ {
    q.split('&').filter_map(|kv| {
        let mut it = kv.splitn(2, '=');
        let k = it.next()?.to_string();
        let v = it.next().unwrap_or("").to_string();
        Some((k, v))
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn entry(name: &str, is_dir: bool, size: u64) -> EntryMeta {
        EntryMeta {
            name: name.to_string(),
            is_dir,
            size,
            mtime: UNIX_EPOCH + Duration::from_secs(1749103800),
        }
    }

    fn state(read_only: bool) -> FileServerState {
        FileServerState {
            root: std::path::PathBuf::from("/tmp"),
            read_only,
            auth: None,
        }
    }

    fn hm_get(name: &'static str, value: &str) -> axum::http::HeaderMap {
        let mut m = axum::http::HeaderMap::new();
        m.insert(name, value.parse().unwrap());
        m
    }

    // --- classify ---

    #[test]
    fn test_classify_dirs() {
        assert_eq!(classify("Documents", true), "folder");
        assert_eq!(classify("anything", true), "folder");
    }

    #[test]
    fn test_classify_pdf() {
        assert_eq!(classify("report.pdf", false), "pdf");
        assert_eq!(classify("Report.PDF", false), "pdf");
    }

    #[test]
    fn test_classify_images() {
        assert_eq!(classify("a.jpg", false), "img");
        assert_eq!(classify("b.JPEG", false), "img");
        assert_eq!(classify("c.png", false), "img");
        assert_eq!(classify("d.webp", false), "img");
    }

    #[test]
    fn test_classify_docs() {
        assert_eq!(classify("notes.md", false), "doc");
        assert_eq!(classify("report.docx", false), "doc");
        assert_eq!(classify("a.txt", false), "doc");
    }

    #[test]
    fn test_classify_archives() {
        assert_eq!(classify("x.zip", false), "zip");
        assert_eq!(classify("y.tar.gz", false), "zip");
        assert_eq!(classify("z.7z", false), "zip");
    }

    #[test]
    fn test_classify_code() {
        assert_eq!(classify("main.rs", false), "code");
        assert_eq!(classify("index.js", false), "code");
        assert_eq!(classify("a.py", false), "code");
        assert_eq!(classify("b.tsx", false), "code");
        assert_eq!(classify("c.json", false), "code");
    }

    #[test]
    fn test_classify_audio_video() {
        assert_eq!(classify("a.mp3", false), "audio");
        assert_eq!(classify("a.wav", false), "audio");
        assert_eq!(classify("b.mp4", false), "video");
        assert_eq!(classify("b.mkv", false), "video");
    }

    #[test]
    fn test_classify_unknown_is_other() {
        assert_eq!(classify("random.unknownext", false), "other");
        assert_eq!(classify("noext", false), "other");
    }

    // --- wants_viewer ---

    #[test]
    fn test_wants_viewer_default_no_accept() {
        let m = axum::http::HeaderMap::new();
        assert!(!wants_viewer(&m, None), "*/* default → autoindex");
    }

    #[test]
    fn test_wants_viewer_browser_accept() {
        let m = hm_get(
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        );
        assert!(wants_viewer(&m, None));
    }

    #[test]
    fn test_wants_viewer_explicit_text_html() {
        let m = hm_get("accept", "text/html");
        assert!(wants_viewer(&m, None));
    }

    #[test]
    fn test_wants_viewer_query_param_overrides() {
        let m = axum::http::HeaderMap::new();
        assert!(wants_viewer(&m, Some("view=ui")));
        // view=raw takes precedence regardless of position in the query.
        assert!(
            !wants_viewer(&m, Some("view=raw&foo=bar")),
            "view=raw → autoindex"
        );
        assert!(!wants_viewer(&m, Some("view=raw")));
        assert!(!wants_viewer(&m, Some("foo=bar&view=raw")));
        assert!(wants_viewer(&m, Some("foo=bar&view=html")));
    }

    #[test]
    fn test_wants_viewer_reqwest_star_star() {
        let m = hm_get("accept", "*/*");
        assert!(!wants_viewer(&m, None), "reqwest default */* → autoindex");
    }

    // --- render ---

    #[test]
    fn test_render_viewer_embeds_data() {
        let entries = vec![entry("docs", true, 0), entry("a.pdf", false, 100)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/files");
        assert!(html.contains("window.__DATA__ = "));
        assert!(html.contains("\"path\":\"/files\""));
        assert!(html.contains("\"read_only\":false"));
        assert!(html.contains("\"name\":\"a.pdf\""));
        assert!(html.contains("\"kind\":\"pdf\""));
        assert!(html.contains("\"name\":\"docs\""));
        assert!(html.contains("\"kind\":\"folder\""));
    }

    #[test]
    fn test_render_viewer_read_only_flag() {
        let s = state(true);
        let html = render_viewer_html(&s, &[], "/");
        assert!(html.contains("\"read_only\":true"));
    }

    #[test]
    fn test_render_viewer_escapes_script_close() {
        // XSS guard: a filename with "</script>" must not break out of the data block.
        let entries = vec![entry("</script><img src=x>", false, 1)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/");
        // The escaped sequence must appear in the data block.
        assert!(html.contains("<\\/script>"), "must escape </ to <\\/");
        // Extract only the JSON value (between `= ` and the trailing `;`).
        let marker = "window.__DATA__ = ";
        let data_start = html.find(marker).unwrap() + marker.len();
        let data_end = html[data_start..].find(';').unwrap() + data_start;
        let json_value = &html[data_start..data_end];
        // The literal `</script>` must NOT appear in the JSON value.
        assert!(
            !json_value.contains("</script>"),
            "raw </script> in JSON value"
        );
    }

    #[test]
    fn test_render_viewer_href_url_encoded() {
        let entries = vec![entry("hello world.txt", false, 10)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/");
        assert!(html.contains("hello%20world.txt"));
    }

    #[test]
    fn test_render_viewer_dir_href_has_trailing_slash() {
        let entries = vec![entry("subdir", true, 0)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/");
        assert!(html.contains("\"href\":\"subdir/\""));
    }

    // --- urlencoding / extension ---

    #[test]
    fn test_urlencoding_safe_chars() {
        assert_eq!(urlencoding("abc-DEF_123.~"), "abc-DEF_123.~");
    }

    #[test]
    fn test_urlencoding_special_chars() {
        assert_eq!(urlencoding("a b"), "a%20b");
        assert_eq!(urlencoding("中"), "%E4%B8%AD"); // UTF-8 bytes percent-encoded
    }

    #[test]
    fn test_extension() {
        assert_eq!(extension("a.b.c"), "c");
        assert_eq!(extension("README"), "");
        assert_eq!(extension(".hidden"), "");
    }

    // --- wants_viewer: query boundary & Accept q=0 (review fixes) ---

    #[test]
    fn test_wants_viewer_query_key_case_insensitive() {
        let m = axum::http::HeaderMap::new();
        // key match must be case-insensitive (parity with value lowercase)
        assert!(wants_viewer(&m, Some("VIEW=ui")));
        assert!(wants_viewer(&m, Some("View=html")));
        assert!(!wants_viewer(&m, Some("View=raw")));
    }

    #[test]
    fn test_wants_viewer_query_empty_value_falls_through() {
        // ?view= (empty) doesn't trigger viewer or autoindex — fall through to Accept.
        let m = hm_get("accept", "text/html");
        assert!(wants_viewer(&m, Some("view=")));
        let m = axum::http::HeaderMap::new();
        assert!(!wants_viewer(&m, Some("view=")));
    }

    #[test]
    fn test_wants_viewer_query_malformed_no_panic() {
        let m = axum::http::HeaderMap::new();
        // Bare &, unkeyed tokens, missing values — must not crash.
        assert!(!wants_viewer(&m, Some("&")));
        assert!(!wants_viewer(&m, Some("&&&")));
        assert!(!wants_viewer(&m, Some("foo")));
        assert!(!wants_viewer(&m, Some("=ui")));
        assert!(!wants_viewer(&m, Some("view")));
    }

    #[test]
    fn test_wants_viewer_query_repeated_key_first_wins() {
        // First matching key wins (deterministic, even if unusual).
        let m = axum::http::HeaderMap::new();
        assert!(wants_viewer(&m, Some("view=ui&view=raw")));
        assert!(!wants_viewer(&m, Some("view=raw&view=ui")));
    }

    #[test]
    fn test_wants_viewer_accept_q0_explicit_reject() {
        // RFC 7231 §5.3.2: "Accept: text/html;q=0" must NOT serve HTML.
        let m = hm_get("accept", "text/html;q=0");
        assert!(!wants_viewer(&m, None));
    }

    #[test]
    fn test_wants_viewer_accept_q0_with_other_media() {
        // Defensive P2P Accept like "text/html;q=0, */*;q=1" must not be served viewer.
        let m = hm_get("accept", "text/html;q=0, */*;q=1");
        assert!(!wants_viewer(&m, None));
    }

    #[test]
    fn test_wants_viewer_accept_q_positive_values() {
        let m = hm_get("accept", "text/html;q=0.5");
        assert!(wants_viewer(&m, None));
        let m = hm_get("accept", "text/html;q=1.0");
        assert!(wants_viewer(&m, None));
    }

    #[test]
    fn test_wants_viewer_accept_q_zero_among_others() {
        // text/html;q=0 doesn't poison the *other* text/html entries.
        // We only need ONE surviving positive match to serve viewer.
        let m = hm_get("accept", "text/html;q=0, application/xhtml+xml, text/html");
        assert!(wants_viewer(&m, None));
    }

    // --- classify: trailing-dot / empty / odd inputs (review fixes) ---

    #[test]
    fn test_classify_trailing_dot_and_empty() {
        // "foo." has no real extension (extension() guards i+1<len).
        assert_eq!(classify("foo.", false), "other");
        // Empty filename → empty extension → "other".
        assert_eq!(classify("", false), "other");
    }

    // --- urlencoding: critical separators (review fix) ---

    #[test]
    fn test_urlencoding_separators() {
        // Query / path / fragment / form separators must be percent-encoded,
        // otherwise a filename "a&b.txt" would corrupt the query string.
        assert_eq!(urlencoding("a&b"), "a%26b");
        assert_eq!(urlencoding("a?b"), "a%3Fb");
        assert_eq!(urlencoding("a#b"), "a%23b");
        assert_eq!(urlencoding("a=b"), "a%3Db");
        assert_eq!(urlencoding("a+b"), "a%2Bb");
        assert_eq!(urlencoding("a/b"), "a%2Fb");
    }

    // --- render: empty entries + XSS vectors beyond </script> (review fixes) ---

    #[test]
    fn test_render_viewer_empty_entries_array() {
        let s = state(false);
        let html = render_viewer_html(&s, &[], "/");
        // Lock the "empty list" shape — guards against accidental single-null collapse.
        assert!(html.contains("\"entries\":[]"));
    }

    #[test]
    fn test_render_viewer_xss_full_script_payload() {
        // <script>alert(1)</script> in a name — the embedded </ must be escaped
        // even though the surrounding < and > survive (JSON handles them safely
        // as long as the </ sequence never appears in the value).
        let entries = vec![entry("<script>alert(1)</script>", false, 1)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/");
        let marker = "window.__DATA__ = ";
        let data_start = html.find(marker).unwrap() + marker.len();
        let data_end = html[data_start..].find(';').unwrap() + data_start;
        let json_value = &html[data_start..data_end];
        assert!(!json_value.contains("</script>"));
        assert!(html.contains("<\\/script>"));
    }

    #[test]
    fn test_render_viewer_xss_svg_onload() {
        // Attribute-style injection via filename; same </ guard applies.
        let entries = vec![entry("a</style><img onerror=alert(1) src=x>", false, 1)];
        let s = state(false);
        let html = render_viewer_html(&s, &entries, "/");
        let marker = "window.__DATA__ = ";
        let data_start = html.find(marker).unwrap() + marker.len();
        let data_end = html[data_start..].find(';').unwrap() + data_start;
        let json_value = &html[data_start..data_end];
        assert!(!json_value.contains("</style>"));
        assert!(html.contains("<\\/style>"));
    }

    // --- format_iso8601 / url_query_pairs: direct coverage (review fixes) ---

    #[test]
    fn test_format_iso8601_known() {
        // 1749103800 → 2025-06-05T06:10:00Z
        assert_eq!(format_iso8601(UNIX_EPOCH + Duration::from_secs(1749103800)),
                   "2025-06-05T06:10:00Z");
    }

    #[test]
    fn test_format_iso8601_pre_epoch_is_empty() {
        // Pre-1970 timestamp: duration_since returns Err, expect empty string.
        let t = std::time::SystemTime::UNIX_EPOCH - Duration::from_secs(60);
        assert_eq!(format_iso8601(t), "");
    }

    #[test]
    fn test_url_query_pairs_basic() {
        let pairs: Vec<_> = url_query_pairs("a=1&b=2&c").collect();
        assert_eq!(pairs, vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
            ("c".to_string(), "".to_string()),
        ]);
    }
}
