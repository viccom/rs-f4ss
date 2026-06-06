//! Nginx-format autoindex HTML generation.
//!
//! Output is designed to be parseable by rs-f4ss's own `HttpBackend::parse_autoindex()`,
//! ensuring client-server round-trip compatibility.

use std::time::SystemTime;

use super::EntryMeta;

/// Generate nginx-style autoindex HTML for a directory listing.
pub fn generate_autoindex(entries: &[EntryMeta], dir_path: &str) -> String {
    let escaped_path = html_escape_text(dir_path);
    let title = format!("Index of {escaped_path}/");
    let mut html = String::with_capacity(4096);
    html.push_str("<html>\n<head><title>");
    html.push_str(&title);
    html.push_str("</title></head>\n<body>\n<h1>");
    html.push_str(&title);
    html.push_str("</h1>\n<hr><pre>\n");

    // Parent directory link (always present except for root)
    if dir_path != "/" && !dir_path.is_empty() {
        html.push_str("<a href=\"../\">../</a>\n");
    }

    for entry in entries {
        let href = if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };

        // Format: <a href="name">display_name</a>  padding  DD-Mon-YYYY HH:MM  size
        let display = if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };

        let date_str = format_date(entry.mtime);
        let size_str = if entry.is_dir {
            "-".to_string()
        } else {
            entry.size.to_string()
        };

        // Align columns: name column 50 chars, date 20 chars
        let name_col = format!(
            "<a href=\"{}\">{}</a>",
            html_escape_attr(&href),
            html_escape_text(&display)
        );
        let padding = if name_col.len() < 50 {
            " ".repeat(50 - name_col.len())
        } else {
            "  ".to_string()
        };

        html.push_str(&name_col);
        html.push_str(&padding);
        html.push_str(&date_str);
        html.push_str(&format!("{:>12}", size_str));
        html.push('\n');
    }

    html.push_str("</pre><hr>\n</body>\n</html>");

    html
}

/// Format SystemTime as DD-Mon-YYYY HH:MM (nginx autoindex style).
fn format_date(t: SystemTime) -> String {
    let dur = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    chrono::DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
        .map(|dt| dt.format("%d-%b-%Y %H:%M").to_string())
        .unwrap_or_else(|| "                  ".to_string())
}

fn html_escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_entry(name: &str, is_dir: bool, size: u64) -> EntryMeta {
        EntryMeta {
            name: name.to_string(),
            is_dir,
            size,
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1749103800), // 2025-06-05 10:30:00 UTC
        }
    }

    #[test]
    fn test_generate_basic() {
        let entries = vec![
            make_entry("documents", true, 0),
            make_entry("readme.txt", false, 2048),
        ];
        let html = generate_autoindex(&entries, "/files");
        assert!(html.contains("Index of /files/"));
        assert!(html.contains("<a href=\"documents/\">documents/</a>"));
        assert!(html.contains("<a href=\"readme.txt\">readme.txt</a>"));
        assert!(html.contains("2048"));
    }

    #[test]
    fn test_generate_parent_link() {
        let entries = vec![make_entry("file.txt", false, 100)];
        let html = generate_autoindex(&entries, "/sub");
        assert!(html.contains("<a href=\"../\">../</a>"));
    }

    #[test]
    fn test_generate_no_parent_at_root() {
        let entries = vec![make_entry("file.txt", false, 100)];
        let html = generate_autoindex(&entries, "/");
        assert!(!html.contains("../"));
    }

    #[test]
    fn test_html_escaping() {
        let entries = vec![make_entry("a&b.txt", false, 42)];
        let html = generate_autoindex(&entries, "/");
        assert!(html.contains("a&amp;b.txt"));
    }

    /// P0: Round-trip test — server HTML must be parseable by client's parse_autoindex()
    #[test]
    fn test_autoindex_round_trip() {
        // Import the client parser from the http backend
        // Note: this test requires the 'http' feature
        #[cfg(feature = "http")]
        {
            use crate::backend::http::parse_autoindex;

            let entries = vec![
                make_entry("documents", true, 0),
                make_entry("readme.txt", false, 2048),
                make_entry("photo.jpg", false, 524288),
            ];
            let html = generate_autoindex(&entries, "/files");
            let parsed = parse_autoindex(&html, "/files");

            assert_eq!(parsed.len(), 3, "Should parse exactly 3 entries");

            let docs = parsed
                .iter()
                .find(|e| e.name == "documents")
                .expect("documents entry");
            assert!(docs.dir);
            assert_eq!(docs.path, "/files/documents");

            let readme = parsed
                .iter()
                .find(|e| e.name == "readme.txt")
                .expect("readme entry");
            assert!(!readme.dir);
            assert_eq!(readme.size, 2048);

            let photo = parsed
                .iter()
                .find(|e| e.name == "photo.jpg")
                .expect("photo entry");
            assert!(!photo.dir);
            assert_eq!(photo.size, 524288);
        }

        #[cfg(not(feature = "http"))]
        {
            // When http feature is not enabled, just verify HTML generation works
            let entries = vec![make_entry("file.txt", false, 100)];
            let html = generate_autoindex(&entries, "/");
            assert!(!html.is_empty());
        }
    }

    #[test]
    fn test_autoindex_round_trip_special_chars() {
        #[cfg(feature = "http")]
        {
            use crate::backend::http::parse_autoindex;

            let entries = vec![
                make_entry("hello world.txt", false, 50),
                make_entry("data.csv", false, 100),
            ];
            let html = generate_autoindex(&entries, "/");
            let parsed = parse_autoindex(&html, "/");
            assert_eq!(parsed.len(), 2);
            assert!(parsed.iter().any(|e| e.name == "hello world.txt"));
        }
    }
}
