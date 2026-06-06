//! HTTP static file server backend.
//!
//! Mounts any HTTP server with autoindex (nginx, Apache, Caddy,
//! Python http.server) as a local filesystem. Read-only mode works
//! with zero server config. Read-write requires server-side
//! PUT/DELETE/MKCOL/MOVE support.

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, NaiveDateTime, Utc};
use reqwest::Method;
use std::time::{SystemTime, UNIX_EPOCH};

use super::common::HttpClient;
use super::{Entry, StorageBackend};
use crate::error::BackendError;

pub struct HttpBackend {
    http: HttpClient,
    read_only: bool,
}

impl HttpBackend {
    pub fn from_url(
        url: &str,
        read_only: bool,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, String> {
        let actual_url = if let Some(rest) = url.strip_prefix("statics://") {
            format!("https://{rest}")
        } else if let Some(rest) = url.strip_prefix("static://") {
            format!("http://{rest}")
        } else {
            url.to_string()
        };

        let http = HttpClient::new(&actual_url, username, password).map_err(|e| e.to_string())?;

        Ok(Self { http, read_only })
    }

    fn map_status(path: &str, status: u16) -> Option<BackendError> {
        match status {
            404 => Some(BackendError::NotFound(path.to_string())),
            401 => Some(BackendError::PermissionDenied(
                "Authentication required".into(),
            )),
            403 => Some(BackendError::PermissionDenied(path.to_string())),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Autoindex HTML parsing
// ---------------------------------------------------------------------------

/// Parse autoindex HTML into Entry list. Supports nginx, Apache, Caddy,
/// Python http.server formats. Case-insensitive tag matching.
pub fn parse_autoindex(html: &str, base_path: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let base = base_path.trim_end_matches('/');
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;

    while let Some(link_start) = lower[pos..].find("<a ") {
        let abs_start = pos + link_start;
        let rest_lower = &lower[abs_start..];
        let href_offset = match rest_lower.find("href=") {
            Some(i) => abs_start + i + 5,
            None => {
                pos = abs_start + 3;
                continue;
            }
        };

        // Handle both " and ' as attribute delimiters
        let quote = match html.as_bytes().get(href_offset) {
            Some(b'"') => '"',
            Some(b'\'') => '\'',
            _ => {
                pos = href_offset;
                continue;
            }
        };
        let href_start = href_offset + 1;
        let href_end = match html[href_start..].find(quote) {
            Some(i) => href_start + i,
            None => {
                pos = href_start;
                continue;
            }
        };
        let href = &html[href_start..href_end];

        let href_lower = href.to_ascii_lowercase();
        if href_lower == "../"
            || href_lower == "/"
            || href_lower.starts_with('?')
            || href_lower.starts_with('#')
        {
            pos = href_end + 1;
            continue;
        }

        let after_quote = href_end + 1;
        let tag_close = match html[after_quote..].find('>') {
            Some(i) => after_quote + i + 1,
            None => {
                pos = href_end;
                continue;
            }
        };
        let closing_a = match lower[tag_close..].find("</a>") {
            Some(i) => tag_close + i,
            None => {
                pos = tag_close;
                continue;
            }
        };
        let link_text_raw = html[tag_close..closing_a].trim();
        let link_text = decode_html_entities(link_text_raw);

        if link_text == ".."
            || link_text == "../"
            || link_text.eq_ignore_ascii_case("Parent Directory")
        {
            pos = closing_a + 4;
            continue;
        }

        let is_dir = href.ends_with('/');
        let name = link_text.trim_end_matches('/');
        if name.is_empty() || name == "." {
            pos = closing_a + 4;
            continue;
        }

        let path = if href.starts_with('/') || href_lower.starts_with("http") {
            href.to_string()
        } else {
            format!("{}/{}", base, href.trim_end_matches('/'))
        };

        let after_link = &html[closing_a + 4..];
        let (size, mtime) = parse_line_meta(after_link);

        entries.push(Entry {
            path,
            name: name.to_string(),
            dir: is_dir,
            size,
            mtime,
        });

        pos = closing_a + 4;
    }

    entries
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Max bytes to scan for size/date metadata after a link.
const META_LINE_MAX_LEN: usize = 300;

/// Parse size and date from text following `</a>` until next `</tr>` or newline.
fn parse_line_meta(text: &str) -> (u64, SystemTime) {
    let end = text
        .find("</tr>")
        .unwrap_or_else(|| text.find('\n').unwrap_or(text.len()))
        .min(META_LINE_MAX_LEN);
    let chunk = text[..end].replace("&nbsp;", " ");

    let plain = strip_html_tags(&chunk);
    let line = plain.trim();

    let mut size: u64 = 0;
    let mut mtime = UNIX_EPOCH;

    let tokens: Vec<&str> = line.split_whitespace().collect();

    for i in 0..tokens.len().saturating_sub(1) {
        let is_date = is_nginx_date(tokens[i]) || is_apache_date(tokens[i]);

        if is_date && i + 1 < tokens.len() {
            if let Ok(dt) = parse_flexible_date(tokens[i], tokens.get(i + 1).copied()) {
                mtime = dt;
            }
            if let Some(size_str) = tokens.get(i + 2) {
                size = parse_size(size_str);
            }
            break;
        }
    }

    (size, mtime)
}

/// DD-Mon-YYYY (nginx): 11 chars, '-' at pos 2 and 6.
fn is_nginx_date(s: &str) -> bool {
    s.len() == 11 && s.as_bytes().get(2) == Some(&b'-') && s.as_bytes().get(6) == Some(&b'-')
}

/// YYYY-MM-DD (Apache/Caddy): 10 chars, '-' at pos 4 and 7.
fn is_apache_date(s: &str) -> bool {
    s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-')
}

fn parse_flexible_date(date: &str, time: Option<&str>) -> Result<SystemTime, ()> {
    let normalized = if is_nginx_date(date) {
        let day: u32 = date[..2].parse().map_err(|_| ())?;
        let month_str = &date[3..6];
        let year: i32 = date[7..11].parse().map_err(|_| ())?;
        let month = month_num(month_str)?;
        format!("{year:04}-{month:02}-{day:02}")
    } else {
        date.to_string()
    };

    let combined = match time {
        Some(t) => format!("{normalized} {t}"),
        None => normalized.clone(),
    };

    if let Ok(dt) = NaiveDateTime::parse_from_str(&combined, "%Y-%m-%d %H:%M") {
        return Ok(super::common::datetime_to_systemtime(&dt.and_utc()));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&normalized, "%Y-%m-%d") {
        return Ok(super::common::datetime_to_systemtime(
            &d.and_hms_opt(0, 0, 0).unwrap().and_utc(),
        ));
    }
    Err(())
}

fn month_num(s: &str) -> Result<u32, ()> {
    match s {
        "Jan" => Ok(1),
        "Feb" => Ok(2),
        "Mar" => Ok(3),
        "Apr" => Ok(4),
        "May" => Ok(5),
        "Jun" => Ok(6),
        "Jul" => Ok(7),
        "Aug" => Ok(8),
        "Sep" => Ok(9),
        "Oct" => Ok(10),
        "Nov" => Ok(11),
        "Dec" => Ok(12),
        _ => Err(()),
    }
}

fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' ');
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    if s == "-" || s.is_empty() {
        return 0;
    }
    let s_lower = s.to_ascii_lowercase();
    if let Some(num) = s_lower.strip_suffix('k') {
        return num.parse::<f64>().map(|v| (v * 1024.0) as u64).unwrap_or(0);
    }
    if let Some(num) = s_lower.strip_suffix('m') {
        return num
            .parse::<f64>()
            .map(|v| (v * 1024.0 * 1024.0) as u64)
            .unwrap_or(0);
    }
    if let Some(num) = s_lower.strip_suffix('g') {
        return num
            .parse::<f64>()
            .map(|v| (v * 1024.0 * 1024.0 * 1024.0) as u64)
            .unwrap_or(0);
    }
    s.parse().unwrap_or(0)
}

fn extract_name(path: &str) -> String {
    let p = path.trim_end_matches('/');
    p.rsplit('/').next().unwrap_or(p).to_string()
}

// ---------------------------------------------------------------------------
// StorageBackend implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl StorageBackend for HttpBackend {
    fn protocol(&self) -> &str {
        "http"
    }

    fn server_addr(&self) -> &str {
        self.http.base_url.as_str()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        let list_path = if path.ends_with('/') || path == "/" {
            path.to_string()
        } else {
            format!("{path}/")
        };
        let url = self.http.build_url(&list_path)?;
        let resp = self
            .http
            .send_with_retry(Method::GET, &url, vec![], None)
            .await?;
        let status = resp.status().as_u16();
        if let Some(e) = Self::map_status(path, status) {
            return Err(e);
        }
        if !resp.status().is_success() {
            return Err(BackendError::Internal(format!("GET list failed: {status}")));
        }

        let html = resp
            .text()
            .await
            .map_err(|e| BackendError::Internal(format!("Read response: {e}")))?;

        Ok(parse_autoindex(&html, &list_path))
    }

    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        let is_dir = path.ends_with('/') || path == "/";
        let name = extract_name(path);

        if is_dir {
            let url = self.http.build_url(path)?;
            // Try HEAD first (no body transfer)
            let resp = self
                .http
                .send_with_retry(Method::HEAD, &url, vec![], None)
                .await?;
            if resp.status().is_success() {
                return Ok(Entry {
                    path: path.to_string(),
                    name,
                    dir: true,
                    size: 0,
                    mtime: UNIX_EPOCH,
                });
            }
            // Some servers don't support HEAD on directories — try GET
            let resp = self
                .http
                .send_with_retry(Method::GET, &url, vec![], None)
                .await?;
            let status = resp.status().as_u16();
            if let Some(e) = Self::map_status(path, status) {
                super::common::drain_response(resp).await;
                return Err(e);
            }
            if !resp.status().is_success() {
                super::common::drain_response(resp).await;
                return Err(BackendError::NotFound(path.to_string()));
            }
            super::common::drain_response(resp).await;
            return Ok(Entry {
                path: path.to_string(),
                name,
                dir: true,
                size: 0,
                mtime: UNIX_EPOCH,
            });
        }

        // File stat: HEAD gives Content-Length and Last-Modified
        let url = self.http.build_url(path)?;
        let resp = self
            .http
            .send_with_retry(Method::HEAD, &url, vec![], None)
            .await?;
        let status = resp.status().as_u16();

        if status == 404 {
            // Maybe it's a directory — try with trailing slash
            let dir_url = format!("{}/", url.trim_end_matches('/'));
            let dir_resp = self
                .http
                .send_with_retry(Method::HEAD, &dir_url, vec![], None)
                .await?;
            if dir_resp.status().is_success() {
                return Ok(Entry {
                    path: format!("{path}/"),
                    name,
                    dir: true,
                    size: 0,
                    mtime: UNIX_EPOCH,
                });
            }
            return Err(BackendError::NotFound(path.to_string()));
        }

        if let Some(e) = Self::map_status(path, status) {
            return Err(e);
        }

        let size = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let mtime = resp
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| DateTime::parse_from_rfc2822(v).ok())
            .map(|dt| super::common::datetime_to_systemtime(&dt.with_timezone(&Utc)))
            .unwrap_or(UNIX_EPOCH);

        Ok(Entry {
            path: path.to_string(),
            name,
            dir: false,
            size,
            mtime,
        })
    }

    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError> {
        let url = self.http.build_url(path)?;

        if let Some(end_inc) = offset
            .checked_add(u64::from(size))
            .and_then(|e| e.checked_sub(1))
        {
            let range = format!("bytes={offset}-{end_inc}");
            let resp = self
                .http
                .send_with_retry(Method::GET, &url, vec![("Range", range)], None)
                .await?;
            let status = resp.status().as_u16();

            if status == 206 {
                return resp
                    .bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| BackendError::Internal(format!("Read: {e}")));
            }
            if status == 404 {
                return Err(BackendError::NotFound(path.to_string()));
            }
            if status == 416 {
                drop(resp);
                return self.http.read_full_and_slice(&url, offset, size).await;
            }
            if let Some(e) = Self::map_status(path, status) {
                return Err(e);
            }
            if status == 200 {
                let data = resp
                    .bytes()
                    .await
                    .map(|b| b.to_vec())
                    .map_err(|e| BackendError::Internal(format!("Read: {e}")))?;
                let start = match usize::try_from(offset) {
                    Ok(s) => s,
                    Err(_) => return Ok(Vec::new()),
                };
                if start >= data.len() {
                    return Ok(Vec::new());
                }
                let end = start.saturating_add(size as usize).min(data.len());
                return Ok(data[start..end].to_vec());
            }
        }

        self.http.read_full_and_slice(&url, offset, size).await
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<(), BackendError> {
        if self.read_only {
            return Err(BackendError::ReadOnly);
        }
        let url = self.http.build_url(path)?;
        let resp = self
            .http
            .send_with_retry(
                Method::PUT,
                &url,
                vec![],
                Some(Bytes::copy_from_slice(data)),
            )
            .await?;
        let status = resp.status().as_u16();
        if let Some(e) = Self::map_status(path, status) {
            return Err(e);
        }
        if !resp.status().is_success() && status != 201 && status != 204 {
            return Err(BackendError::Internal(format!("PUT failed: {status}")));
        }
        Ok(())
    }

    async fn mkdir(&self, path: &str) -> Result<(), BackendError> {
        if self.read_only {
            return Err(BackendError::ReadOnly);
        }
        let url = self.http.build_url(path)?;
        let resp = self
            .http
            .send_with_retry(Method::from_bytes(b"MKCOL").unwrap(), &url, vec![], None)
            .await?;
        let status = resp.status().as_u16();
        if let Some(e) = Self::map_status(path, status) {
            return Err(e);
        }
        if !resp.status().is_success() && status != 201 && status != 405 {
            return Err(BackendError::Internal(format!("MKCOL failed: {status}")));
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<(), BackendError> {
        if self.read_only {
            return Err(BackendError::ReadOnly);
        }
        let url = self.http.build_url(path)?;
        let resp = self
            .http
            .send_with_retry(Method::DELETE, &url, vec![], None)
            .await?;
        let status = resp.status().as_u16();
        if let Some(e) = Self::map_status(path, status) {
            return Err(e);
        }
        if !resp.status().is_success() && status != 204 {
            return Err(BackendError::Internal(format!("DELETE failed: {status}")));
        }
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), BackendError> {
        if self.read_only {
            return Err(BackendError::ReadOnly);
        }
        let url = self.http.build_url(from)?;
        let dest = self.http.build_url(to)?;
        let resp = self
            .http
            .send_with_retry(
                Method::from_bytes(b"MOVE").unwrap(),
                &url,
                vec![("Destination", dest), ("Overwrite", "T".to_string())],
                None,
            )
            .await?;
        let status = resp.status().as_u16();
        if let Some(e) = Self::map_status(from, status) {
            return Err(e);
        }
        if !resp.status().is_success() && status != 201 && status != 204 {
            return Err(BackendError::Internal(format!("MOVE failed: {status}")));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nginx_autoindex() {
        let html = r#"<html><head><title>Index of /files/</title></head>
<body><h1>Index of /files/</h1><hr><pre><a href="../">../</a>
<a href="documents/">documents/</a>                                        02-Jun-2026 10:30                   -
<a href="readme.txt">readme.txt</a>                                        01-Jun-2026 08:15                 2048
<a href="photo.jpg">photo.jpg</a>                                          30-May-2026 14:22              524288
</pre><hr></body></html>"#;

        let entries = parse_autoindex(html, "/files");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "documents");
        assert!(entries[0].dir);
        assert_eq!(entries[1].name, "readme.txt");
        assert!(!entries[1].dir);
        assert_eq!(entries[1].size, 2048);
        assert_eq!(entries[2].name, "photo.jpg");
        assert_eq!(entries[2].size, 524288);
    }

    #[test]
    fn test_parse_apache_autoindex() {
        let html = r#"<table>
<tr><td><a href="../">Parent Directory</a></td><td>&nbsp;</td><td>-</td></tr>
<tr><td><a href="documents/">documents/</a></td><td>2026-06-02 10:30</td><td>-</td></tr>
<tr><td><a href="readme.txt">readme.txt</a></td><td>2026-06-01 08:15</td><td>2.0K</td></tr>
</table>"#;

        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 2);
        assert!(entries[0].dir);
        assert_eq!(entries[0].name, "documents");
        assert_eq!(entries[1].name, "readme.txt");
        assert_eq!(entries[1].size, 2048);
    }

    #[test]
    fn test_parse_python_autoindex() {
        let html = r#"<body>
<h1>Directory listing for /</h1>
<hr>
<ul>
<li><a href="file.txt">file.txt</a></li>
<li><a href="subdir/">subdir/</a></li>
<li><a href="data.csv">data.csv</a></li>
</ul>
<hr>
</body>"#;

        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "file.txt");
        assert!(!entries[0].dir);
        assert_eq!(entries[1].name, "subdir");
        assert!(entries[1].dir);
        assert_eq!(entries[2].name, "data.csv");
        assert_eq!(entries[0].size, 0);
    }

    #[test]
    fn test_parse_uppercase_tags() {
        let html = r#"<HTML><BODY>
<H1>Index</H1>
<PRE><A HREF="file.txt">file.txt</A>   01-Jun-2026 10:00  1024
<A HREF="dir/">dir/</A>   02-Jun-2026 12:00  -
</PRE></BODY></HTML>"#;

        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "file.txt");
        assert_eq!(entries[0].size, 1024);
        assert_eq!(entries[1].name, "dir");
        assert!(entries[1].dir);
    }

    #[test]
    fn test_parse_single_quote_href() {
        let html = r#"<pre>
<a href='doc.pdf'>doc.pdf</a>
<a href='images/'>images/</a>
</pre>"#;

        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "doc.pdf");
        assert_eq!(entries[1].name, "images");
        assert!(entries[1].dir);
    }

    #[test]
    fn test_parse_html_entities_in_name() {
        let html = r#"<pre>
<a href="a&amp;b.txt">a&amp;b.txt</a>
<a href="report.pdf">report.pdf</a>
</pre>"#;

        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a&b.txt");
        assert_eq!(entries[1].name, "report.pdf");
    }

    #[test]
    fn test_parse_minimal_links() {
        let html = r#"<pre>
<a href="a.txt">a.txt</a>
<a href="b.dat">b.dat</a>
</pre>"#;
        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[1].name, "b.dat");
    }

    #[test]
    fn test_parse_skips_parent() {
        let html = r#"<a href="../">../</a><a href="file.txt">file.txt</a>"#;
        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
    }

    #[test]
    fn test_parse_skips_query_links() {
        let html = r#"<a href="?C=N;O=D">Name</a><a href="file.txt">file.txt</a>"#;
        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_skips_parent_directory_text() {
        let html = r#"<a href="/">Parent Directory</a><a href="f.txt">f.txt</a>"#;
        let entries = parse_autoindex(html, "/");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "f.txt");
    }

    #[test]
    fn test_parse_empty_dir() {
        let html = r#"<pre><a href="../">../</a></pre>"#;
        let entries = parse_autoindex(html, "/");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_size_suffixes() {
        assert_eq!(parse_size("1024"), 1024);
        assert_eq!(parse_size("2.0K"), 2048);
        assert_eq!(parse_size("1.5M"), 1572864);
        assert_eq!(parse_size("1G"), 1073741824);
        assert_eq!(parse_size("-"), 0);
        assert_eq!(parse_size(""), 0);
    }

    #[test]
    fn test_parse_date_nginx() {
        let mtime = parse_flexible_date("02-Jun-2026", Some("10:30")).unwrap();
        assert_ne!(mtime, UNIX_EPOCH);
    }

    #[test]
    fn test_parse_date_apache() {
        let mtime = parse_flexible_date("2026-06-02", Some("10:30")).unwrap();
        assert_ne!(mtime, UNIX_EPOCH);
    }

    #[test]
    fn test_parse_date_dateonly() {
        let mtime = parse_flexible_date("2026-06-02", None).unwrap();
        assert_ne!(mtime, UNIX_EPOCH);
    }

    #[test]
    fn test_from_url_basic() {
        let backend = HttpBackend::from_url("http://host:9000", false, None, None).unwrap();
        assert_eq!(backend.protocol(), "http");
        assert!(!backend.is_read_only());
    }

    #[test]
    fn test_from_url_static_scheme() {
        let backend = HttpBackend::from_url("static://host:9000", true, None, None).unwrap();
        assert!(backend.is_read_only());
        assert!(backend.http.base_url.as_str().starts_with("http://"));
    }

    #[test]
    fn test_from_url_statics_scheme() {
        let backend = HttpBackend::from_url("statics://host:9000", true, None, None).unwrap();
        assert!(backend.is_read_only());
        assert!(backend.http.base_url.as_str().starts_with("https://"));
    }

    #[test]
    fn test_from_url_auth() {
        let backend =
            HttpBackend::from_url("http://host:9000", false, Some("user"), Some("pass")).unwrap();
        assert!(backend.http.auth_header.is_some());
    }

    #[test]
    fn test_from_url_auth_partial_fails() {
        let result = HttpBackend::from_url("http://host:9000", false, Some("user"), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_url() {
        let backend = HttpBackend::from_url("http://host:9000", false, None, None).unwrap();
        let url = backend.http.build_url("/file.txt").unwrap();
        assert_eq!(url, "http://host:9000/file.txt");
    }

    #[test]
    fn test_build_url_traversal_rejected() {
        let backend = HttpBackend::from_url("http://host:9000", false, None, None).unwrap();
        assert!(backend.http.build_url("/../../../etc/passwd").is_err());
    }

    #[test]
    fn test_build_url_null_rejected() {
        let backend = HttpBackend::from_url("http://host:9000", false, None, None).unwrap();
        assert!(backend.http.build_url("/file\x00.txt").is_err());
    }

    #[test]
    fn test_extract_name() {
        assert_eq!(extract_name("/path/to/file.txt"), "file.txt");
        assert_eq!(extract_name("/path/to/dir/"), "dir");
        assert_eq!(extract_name("file.txt"), "file.txt");
    }

    #[test]
    fn test_decode_html_entities() {
        assert_eq!(decode_html_entities("a&amp;b"), "a&b");
        assert_eq!(decode_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_html_entities("x&quot;y&quot;z"), "x\"y\"z");
        assert_eq!(decode_html_entities("&#39;hello&#39;"), "'hello'");
        assert_eq!(decode_html_entities("plain"), "plain");
    }
}
