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

    /// Ranged GET for [offset, offset+size) with 416 EOF semantics.
    ///
    /// Same structure as WebDavBackend::ranged_get: a 416 whose
    /// server-reported size still covers `offset` (dufs rejects an
    /// out-of-bounds end instead of clipping it) is retried once with the
    /// length clamped to that size. The retry is an inlined second
    /// `ranged_get_once`, so at most two requests are ever issued — no
    /// recursion, and never a full-download fallback on 416.
    async fn ranged_get(
        &self,
        url: &str,
        path: &str,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, BackendError> {
        // Zero-length read: nothing to fetch — and without this guard a
        // malformed `bytes=<off>-<off-1>` Range would go out.
        if size == 0 {
            return Ok(Vec::new());
        }
        let total = match self.ranged_get_once(url, path, offset, size).await? {
            RangedRead::Data(data) => return Ok(data),
            RangedRead::Unsatisfied { total } => total,
        };
        match total {
            // End-overrun 416: clamp the length to the server-reported size.
            Some(n) if offset < n => {
                let clamped = ((n - offset).min(u64::from(size)) as u32).max(1);
                match self.ranged_get_once(url, path, offset, clamped).await? {
                    RangedRead::Data(data) => Ok(data),
                    // A repeat 416 is answered as EOF, not retried again.
                    RangedRead::Unsatisfied {
                        total: second_total,
                    } => {
                        tracing::warn!(
                            "[read] second 416 after clamped retry, answering EOF \
                             (empty read): url={url} offset={offset} \
                             first_total={total:?} second_total={second_total:?}"
                        );
                        Ok(Vec::new())
                    }
                }
            }
            // offset is at/past EOF, or the server reported no size.
            _ => Ok(Vec::new()),
        }
    }

    /// One ranged GET attempt; a 416 is surfaced to the caller, not handled.
    async fn ranged_get_once(
        &self,
        url: &str,
        path: &str,
        offset: u64,
        size: u32,
    ) -> Result<RangedRead, BackendError> {
        let end_inclusive = offset
            .checked_add(u64::from(size))
            .and_then(|e| e.checked_sub(1));

        let resp = match end_inclusive {
            Some(end_inc) => {
                let range = format!("bytes={offset}-{end_inc}");
                self.http
                    .send_with_retry(Method::GET, url, vec![("Range", range)], None)
                    .await?
            }
            // offset+size overflows the Range grammar: keep the historic
            // plain-GET fallback (bounded by the 30 s request timeout).
            None => {
                return self
                    .http
                    .read_full_and_slice(url, offset, size)
                    .await
                    .map(RangedRead::Data)
            }
        };

        let status = resp.status().as_u16();

        if status == 206 {
            // Validate the start offset in Content-Range before trusting the
            // body (mirror of the webdav check): a misrouted or cached
            // response would otherwise silently serve shifted data.
            if let Some(cr) = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
            {
                if !super::common::content_range_start_matches(cr, offset) {
                    let msg = format!(
                        "server returned wrong range: requested offset {offset}, got `{cr}`"
                    );
                    super::common::drain_response(resp).await;
                    return Err(BackendError::Internal(msg));
                }
            }
            // Header missing → tolerated: the body length is still bounded by
            // reqwest's Content-Length enforcement.
            let data = resp
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BackendError::Internal(format!("Read: {e}")))?;
            return Ok(RangedRead::Data(data));
        }
        if status == 404 {
            super::common::drain_response(resp).await;
            return Err(BackendError::NotFound(path.to_string()));
        }
        if status == 416 {
            // Never fall back to a full download on 416: on a 1 GiB file that
            // path was measured to cost two uncapped GETs (5.4 s stall). Per
            // RFC the server reports the current size in
            // `Content-Range: bytes */<size>`; drain so the connection pools.
            let total = resp
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .and_then(super::common::parse_unsatisfied_size);
            super::common::drain_response(resp).await;
            return Ok(RangedRead::Unsatisfied { total });
        }
        if let Some(e) = Self::map_status(path, status) {
            super::common::drain_response(resp).await;
            return Err(e);
        }
        if !resp.status().is_success() {
            super::common::drain_response(resp).await;
            return Err(BackendError::Internal(format!("GET failed: {status}")));
        }

        // 200: the server ignored Range — stream past [0, offset) then take
        // `size` bytes. The guard counts bytes actually skipped, so it holds
        // whether or not Content-Length is present (chunked responses too).
        const MAX_FALLBACK_SKIP: u64 = 64 * 1024 * 1024; // 64 MiB
        let need = size as usize;
        let skip = match usize::try_from(offset) {
            Ok(s) => s,
            // offset > usize::MAX (32-bit targets): drain so the pooled
            // connection is reused before answering the empty read.
            Err(_) => {
                super::common::drain_response(resp).await;
                return Ok(RangedRead::Data(Vec::new()));
            }
        };

        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut skipped = 0usize;
        let mut buf = Vec::with_capacity(need);

        while let Some(chunk_result) = stream.next().await {
            let chunk =
                chunk_result.map_err(|e| BackendError::Internal(format!("Stream read: {e}")))?;

            let chunk_len = chunk.len();
            if skipped < skip {
                let skip_remaining = skip - skipped;
                if chunk_len <= skip_remaining {
                    skipped += chunk_len;
                    if skipped as u64 > MAX_FALLBACK_SKIP {
                        return Err(BackendError::NotSupported(
                            "Range-ignoring server: fallback skip exceeded 64 MiB cap".into(),
                        ));
                    }
                    continue;
                }
                // Partial skip: take the tail of this chunk
                let useful_start = skip_remaining;
                let useful = &chunk[useful_start..];
                let take = useful.len().min(need - buf.len());
                buf.extend_from_slice(&useful[..take]);
                skipped += useful_start;
                if skipped as u64 > MAX_FALLBACK_SKIP {
                    return Err(BackendError::NotSupported(
                        "Range-ignoring server: fallback skip exceeded 64 MiB cap".into(),
                    ));
                }
            } else {
                let take = chunk.len().min(need - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }

            if buf.len() >= need {
                // Drain remaining stream for connection pool reuse
                while let Some(Ok(_)) = stream.next().await {}
                break;
            }
        }

        Ok(RangedRead::Data(buf))
    }
}

/// Outcome of one ranged GET attempt; a 416 is surfaced, not handled.
enum RangedRead {
    Data(Vec<u8>),
    Unsatisfied { total: Option<u64> },
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
        self.ranged_get(&url, path, offset, size).await
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

// ---------------------------------------------------------------------------
// Fake-server read-path tests
//
// Mirrors webdav.rs read_path_tests: `read()`'s 416 EOF/clamped-retry
// semantics and the 200 fallback skip cap had no in-process regression net.
// Follows the TcpListener template proven in backend/common.rs.
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "http"))]
mod read_path_tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Read one HTTP request off the socket (headers only; bodies unused here).
    fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    fn backend_for_http(addr: std::net::SocketAddr) -> HttpBackend {
        HttpBackend::from_url(&format!("http://{addr}"), false, None, None).unwrap()
    }

    /// Assert the request carried a Range header with the expected bounds.
    fn expect_range(req: &str, offset: u64, size: u32) {
        let end = offset + size as u64 - 1;
        let range = req
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .unwrap_or_else(|| panic!("no Range header in request: {req}"));
        assert!(
            range.contains(&format!("bytes={offset}-{end}")),
            "unexpected Range: {range}"
        );
    }

    /// 416 with `Content-Range: bytes */50` for a read starting past that size:
    /// EOF answer is an empty read — no fallback full download, and the server
    /// must see exactly one request.
    #[tokio::test]
    async fn read_416_at_eof_returns_empty_without_fallback_download() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut requests = 0usize;
            while let Ok((mut stream, _)) = listener.accept() {
                requests += 1;
                let req = read_http_request(&mut stream).unwrap();
                if requests == 1 {
                    expect_range(&req, 100, 5); // file is only 50 bytes
                    stream
                        .write_all(
                            b"HTTP/1.1 416 Range Not Satisfiable\r\n\
                           Content-Range: bytes */50\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    tx.send(()).unwrap();
                } else {
                    panic!("416 must not trigger a second request, got: {req}");
                }
            }
        });
        let backend = backend_for_http(addr);
        let data = backend.read("/f.bin", 100, 5).await.unwrap();
        assert!(data.is_empty());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    /// dufs-style end-overrun 416: file is 50 bytes, `bytes=40-99` is rejected
    /// with `Content-Range: bytes */50` — the client must clamp the length to
    /// the server-reported size and retry once with `bytes=40-49`.
    #[tokio::test]
    async fn read_416_end_overrun_retries_clamped_to_server_size() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s1, _) = listener.accept().unwrap();
            let r1 = read_http_request(&mut s1).unwrap();
            assert!(r1.contains("bytes=40-99"), "first range: {r1}");
            s1.write_all(
                b"HTTP/1.1 416\r\nContent-Range: bytes */50\r\n\
               Content-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            let (mut s2, _) = listener.accept().unwrap();
            let r2 = read_http_request(&mut s2).unwrap();
            assert!(r2.contains("bytes=40-49"), "clamped retry: {r2}");
            s2.write_all(
                b"HTTP/1.1 206 Partial Content\r\n\
               Content-Range: bytes 40-49/50\r\nContent-Length: 10\r\nConnection: close\r\n\r\nABCDEFGHIJ",
            )
            .unwrap();
        });
        let backend = backend_for_http(addr);
        let data = backend.read("/f.bin", 40, 60).await.unwrap();
        assert_eq!(data, b"ABCDEFGHIJ");
    }

    /// 416 without a Content-Range header: the size can't be classified, so
    /// the answer is EOF (empty read) — no fallback full download, and the
    /// server must see exactly one request.
    #[tokio::test]
    async fn read_416_without_content_range_returns_empty() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut requests = 0usize;
            while let Ok((mut stream, _)) = listener.accept() {
                requests += 1;
                let req = read_http_request(&mut stream).unwrap();
                if requests == 1 {
                    expect_range(&req, 60, 5);
                    stream
                        .write_all(
                            b"HTTP/1.1 416 Range Not Satisfiable\r\n\
                           Content-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    tx.send(()).unwrap();
                } else {
                    panic!(
                        "416 without Content-Range must not trigger a second request, got: {req}"
                    );
                }
            }
        });
        let backend = backend_for_http(addr);
        let data = backend.read("/f.bin", 60, 5).await.unwrap();
        assert!(data.is_empty());
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    /// 200 (Range ignored) + chunked response (no Content-Length): the
    /// streaming skip must refuse to skip past the 64 MiB cap. The fake
    /// server keeps sending 1 MiB chunks; once the guard trips the client
    /// disconnects and the server's writes start failing (expected).
    #[tokio::test]
    async fn read_200_fallback_skip_over_cap_is_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let req = read_http_request(&mut stream).unwrap();
            expect_range(&req, 65 * 1024 * 1024, 4096);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            // 1 MiB chunks; 80 MiB total is safely past the 64 MiB skip cap.
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..80 {
                if stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .is_err()
                {
                    return; // client hung up: skip guard tripped
                }
                if stream.write_all(&chunk).is_err() {
                    return;
                }
                if stream.write_all(b"\r\n").is_err() {
                    return;
                }
            }
        });
        let backend = backend_for_http(addr);
        let err = backend
            .read("/f.bin", 65 * 1024 * 1024, 4096)
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("64 MiB"),
            "expected skip-cap error, got: {err}"
        );
    }

    /// A 206 whose Content-Range starts at the wrong offset must surface as
    /// an error, not silently serve shifted data (mirror of the webdav test,
    /// R-B2).
    #[tokio::test]
    async fn read_206_wrong_content_range_start_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let req = read_http_request(&mut s).unwrap();
            expect_range(&req, 10, 5);
            s.write_all(
                b"HTTP/1.1 206\r\nContent-Range: bytes 0-4/100\r\n\
                  Content-Length: 5\r\nConnection: close\r\n\r\nhello",
            )
            .unwrap();
        });
        let backend = backend_for_http(addr);
        let err = backend.read("/f.bin", 10, 5).await.unwrap_err();
        assert!(format!("{err}").contains("wrong range"), "{err}");
    }

    /// Sentinel regression (R-B1): `Content-Range: bytes 100-104/200` must
    /// NOT satisfy a request at offset 10 — the `-` after the start offset
    /// is what keeps a decimal-prefix "match" from passing. Both backends
    /// share content_range_start_matches, so this pins the http call site.
    #[tokio::test]
    async fn read_206_content_range_start_match_requires_dash_sentinel() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let req = read_http_request(&mut s).unwrap();
            expect_range(&req, 10, 5);
            s.write_all(
                b"HTTP/1.1 206\r\nContent-Range: bytes 100-104/200\r\n\
                  Content-Length: 5\r\nConnection: close\r\n\r\nworld",
            )
            .unwrap();
        });
        let backend = backend_for_http(addr);
        let err = backend.read("/f.bin", 10, 5).await.unwrap_err();
        assert!(format!("{err}").contains("wrong range"), "{err}");
    }
}
