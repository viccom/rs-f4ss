use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::Method;
use std::time::UNIX_EPOCH;
use url::Url;

use super::common::HttpClient;
use super::{Entry, StorageBackend};
use crate::error::BackendError;

/// PROPFIND request body template.
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getcontentlength/>
    <D:getlastmodified/>
    <D:resourcetype/>
  </D:prop>
</D:propfind>"#;

pub struct WebDavBackend {
    http: HttpClient,
    read_only: bool,
}

impl WebDavBackend {
    pub fn new(base_url: &str, read_only: bool) -> Result<Self, BackendError> {
        Self::new_with_auth(base_url, read_only, None, None)
    }

    pub fn new_with_auth(
        base_url: &str,
        read_only: bool,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, BackendError> {
        let http = HttpClient::new(base_url, username, password)?;
        Ok(Self { http, read_only })
    }

    pub fn from_url(
        url: &str,
        read_only: bool,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, String> {
        if url.is_empty() {
            return Err("URL is empty".to_string());
        }
        let lower = url.to_lowercase();
        let is_webdav = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("webdav://")
            || lower.starts_with("webdavs://");
        if !is_webdav {
            return Err(format!("Not a WebDAV URL: {url}"));
        }
        let actual_url = if lower.starts_with("webdavs://") {
            format!("https://{}", &url["webdavs://".len()..])
        } else if lower.starts_with("webdav://") {
            format!("http://{}", &url["webdav://".len()..])
        } else {
            url.to_string()
        };
        Self::new_with_auth(&actual_url, read_only, username, password).map_err(|e| e.to_string())
    }

    pub fn build_url(&self, path: &str) -> Result<String, BackendError> {
        self.http.build_url(path)
    }

    async fn propfind(&self, path: &str, depth: u32) -> Result<String, BackendError> {
        let url = self.http.build_url(path)?;
        let resp = self
            .http
            .send_with_retry(
                Method::from_bytes(b"PROPFIND").unwrap(),
                &url,
                vec![
                    ("Depth", depth.to_string()),
                    ("Content-Type", "application/xml".to_string()),
                ],
                Some(Bytes::from_static(PROPFIND_BODY.as_bytes())),
            )
            .await?;

        let status = resp.status();
        if status.as_u16() == 404 {
            super::common::drain_response(resp).await;
            return Err(BackendError::NotFound(path.to_string()));
        }
        if status.as_u16() == 401 {
            super::common::drain_response(resp).await;
            return Err(BackendError::PermissionDenied(
                "Authentication required".to_string(),
            ));
        }
        if status.as_u16() == 403 {
            super::common::drain_response(resp).await;
            return Err(BackendError::PermissionDenied(path.to_string()));
        }

        if status.as_u16() == 200 {
            super::common::drain_response(resp).await;
            return Err(BackendError::ProtocolError(
                "PROPFIND returned HTTP 200 (expected 207 Multi-Status). \
                 If using dufs daemon mode, point to an alias path (e.g. http://host:port/mydir/) \
                 instead of the root URL."
                    .to_string(),
            ));
        }

        if status.as_u16() != 207 {
            super::common::drain_response(resp).await;
            return Err(BackendError::Internal(format!(
                "PROPFIND failed: {status} (expected 207 Multi-Status)"
            )));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| BackendError::Internal(format!("Failed to read response: {e}")))?;
        const MAX_PROPFIND_SIZE: usize = 32 * 1024 * 1024; // 32 MB
        if text.len() > MAX_PROPFIND_SIZE {
            return Err(BackendError::Internal(format!(
                "PROPFIND response too large: {} bytes (max {})",
                text.len(),
                MAX_PROPFIND_SIZE
            )));
        }
        Ok(text)
    }

    /// Send a HEAD request and return Content-Length if present.
    async fn head_content_length(&self, path: &str) -> Result<u64, BackendError> {
        let url = self.http.build_url(path)?;
        let mut req = self.http.client.request(Method::HEAD, &url);
        req = req.timeout(std::time::Duration::from_secs(15));
        if let Some(ref auth) = self.http.auth_header {
            req = req.header("Authorization", auth.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BackendError::ConnectionFailed(format!("HEAD: {e}")))?;
        let status = resp.status().as_u16();
        let result = if status == 404 {
            Err(BackendError::NotFound(path.to_string()))
        } else if !resp.status().is_success() {
            Err(BackendError::Internal(format!("HEAD failed: {status}")))
        } else {
            resp.headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&s| s > 0)
                .ok_or_else(|| BackendError::Internal("No Content-Length in HEAD response".into()))
        };
        // Consume body for connection pool reuse
        if let Ok(bytes) = resp.bytes().await {
            drop(bytes);
        }
        result
    }

    /// stat() with HEAD fallback for size.
    async fn stat_with_size_fallback(&self, path: &str) -> Result<Entry, BackendError> {
        let xml = self.propfind(path, 0).await?;
        let url = self.http.build_url(path)?;
        let url_path = Url::parse(&url)
            .map(|u| u.path().to_string())
            .unwrap_or_else(|_| path.to_string());
        let mut entries = parse_propfind(&xml, &url_path, false)?;
        let mut entry = entries
            .pop()
            .ok_or_else(|| BackendError::NotFound(path.to_string()))?;

        // Some WebDAV servers (e.g. alist proxying cloud storage) don't report
        // content-length in PROPFIND for remote files. Fall back to HEAD request
        // to get the real file size — critical for WinFsp which won't issue read()
        // if file_size is 0.
        if !entry.dir && entry.size == 0 {
            if let Ok(head_size) = self.head_content_length(path).await {
                tracing::debug!(
                    "[stat] size fallback via HEAD: {} → {head_size} for {path}",
                    entry.size
                );
                entry.size = head_size;
            }
        }

        Ok(entry)
    }
}

/// Strip XML namespace prefix: `"D:response"` → `"response"`, `"response"` → `"response"`.
fn local_name(tag: &[u8]) -> String {
    let s = String::from_utf8_lossy(tag);
    if let Some(i) = s.find(':') {
        s[i + 1..].to_string()
    } else {
        s.to_string()
    }
}

/// Extract path from href: handles absolute URLs (`http://host/path/`) → `/path`.
fn href_to_path(href: &str) -> String {
    let trimmed = href.trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Url::parse(trimmed)
            .map(|u| u.path().trim_end_matches('/').to_string())
            .unwrap_or_else(|_| trimmed.to_string())
    } else {
        trimmed.to_string()
    }
}

pub fn parse_propfind(
    xml: &str,
    request_path: &str,
    filter_self: bool,
) -> Result<Vec<Entry>, BackendError> {
    let mut reader = Reader::from_str(xml);

    let mut entries = Vec::new();
    let mut in_response = false;
    let mut in_href = false;
    let mut in_prop = false;
    let mut in_resourcetype = false;
    let mut current_href = String::new();
    let mut current_size: u64 = 0;
    let mut current_mtime = UNIX_EPOCH;
    let mut is_dir = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let tag = local_name(e.name().as_ref());
                match tag.as_str() {
                    "response" => {
                        in_response = true;
                        current_href.clear();
                        current_size = 0;
                        current_mtime = UNIX_EPOCH;
                        is_dir = false;
                    }
                    "href" => in_href = true,
                    "prop" => in_prop = true,
                    "resourcetype" => {
                        in_resourcetype = true;
                    }
                    "collection" if in_resourcetype => {
                        is_dir = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let tag = local_name(e.name().as_ref());
                match tag.as_str() {
                    "response" => {
                        in_response = true;
                        current_href.clear();
                        current_size = 0;
                        current_mtime = UNIX_EPOCH;
                        is_dir = false;
                    }
                    "collection" if in_resourcetype => {
                        is_dir = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_href {
                    current_href = e
                        .unescape()
                        .map_err(|e| BackendError::ProtocolError(format!("XML error: {e}")))?
                        .to_string();
                } else if in_prop {
                    let text = e
                        .unescape()
                        .map_err(|e| BackendError::ProtocolError(format!("XML error: {e}")))?;
                    if text.starts_with("HTTP/") {
                    } else if let Ok(size) = text.parse::<u64>() {
                        current_size = size;
                    } else if let Ok(dt) = DateTime::parse_from_rfc2822(&text) {
                        current_mtime =
                            super::common::datetime_to_systemtime(&dt.with_timezone(&Utc));
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let tag = local_name(e.name().as_ref());
                match tag.as_str() {
                    "response" => {
                        if in_response {
                            let href_path = href_to_path(&current_href);
                            let normalized_request = request_path.trim_end_matches('/');
                            let is_self = is_dir && href_path == normalized_request;
                            tracing::debug!(
                                "[propfind] href={current_href:?} → href_path={href_path:?} vs request={normalized_request:?} is_dir={is_dir} is_self={is_self}"
                            );
                            if !(filter_self && is_self) {
                                let name = extract_name(&current_href);
                                let decoded_path = percent_decode_str(&current_href);
                                entries.push(Entry {
                                    path: decoded_path,
                                    name,
                                    dir: is_dir,
                                    size: current_size,
                                    mtime: current_mtime,
                                });
                            }
                            in_response = false;
                        }
                    }
                    "href" => in_href = false,
                    "prop" => in_prop = false,
                    "resourcetype" => in_resourcetype = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(BackendError::ProtocolError(format!("XML parse error: {e}"))),
            _ => {}
        }
    }

    Ok(entries)
}

fn extract_name(href: &str) -> String {
    let path = href.trim_end_matches('/');
    let raw = path.rsplit('/').next().unwrap_or(path);
    percent_decode_str(raw)
}

fn percent_decode_str(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

#[async_trait]
impl StorageBackend for WebDavBackend {
    fn protocol(&self) -> &str {
        "webdav"
    }

    fn server_addr(&self) -> &str {
        self.http.base_url.as_str()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn list(&self, path: &str) -> Result<Vec<Entry>, BackendError> {
        let xml = self.propfind(path, 1).await?;
        let url = self.http.build_url(path)?;
        let url_path = Url::parse(&url)
            .map(|u| u.path().to_string())
            .unwrap_or_else(|_| path.to_string());
        tracing::debug!("[list] path={path:?} url={url} url_path={url_path:?}");
        let mut entries = parse_propfind(&xml, &url_path, true)?;

        // Post-filter: normalize entry paths and compare against BOTH url_path and fuse_path.
        // Servers may return hrefs as full URLs, with base prefix, or without base prefix.
        // url_path from Url::path() retains percent-encoding for non-ASCII chars,
        // but e.path was decoded by percent_decode_str(). Decode url_path for comparison.
        let url_normalized = percent_decode_str(url_path.trim_end_matches('/'));
        let fuse_normalized = path.trim_end_matches('/');
        let before = entries.len();
        entries.retain(|e| {
            if !e.dir {
                return true;
            }
            let entry_path = href_to_path(&e.path);
            let is_self = entry_path == url_normalized || entry_path == fuse_normalized;
            if is_self {
                tracing::debug!(
                    "[list] filtered self-entry: name={:?} path={:?} normalized={entry_path:?}",
                    e.name,
                    e.path
                );
            }
            !is_self
        });
        tracing::debug!(
            "[list] entries: {before} → {} (after self-filter)",
            entries.len()
        );

        Ok(entries)
    }

    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        self.stat_with_size_fallback(path).await
    }

    async fn read(&self, path: &str, offset: u64, size: u32) -> Result<Vec<u8>, BackendError> {
        let url = self.http.build_url(path)?;
        let end_inclusive = offset
            .checked_add(u64::from(size))
            .and_then(|e| e.checked_sub(1));

        let range_header = end_inclusive.map(|ei| format!("bytes={offset}-{ei}"));

        let mut req_builder = self.http.client.request(Method::GET, &url);
        if let Some(ref auth) = self.http.auth_header {
            req_builder = req_builder.header("Authorization", auth.as_str());
        }
        if let Some(ref rh) = range_header {
            req_builder = req_builder.header("Range", rh);
        }
        req_builder = req_builder.timeout(std::time::Duration::from_secs(300));
        let resp = req_builder
            .send()
            .await
            .map_err(|e| BackendError::ConnectionFailed(format!("GET: {e}")))?;

        let status = resp.status().as_u16();

        if status == 404 {
            super::common::drain_response(resp).await;
            return Err(BackendError::NotFound(path.to_string()));
        }
        if status == 401 {
            super::common::drain_response(resp).await;
            return Err(BackendError::PermissionDenied(
                "Authentication required".to_string(),
            ));
        }
        if status == 403 {
            super::common::drain_response(resp).await;
            return Err(BackendError::PermissionDenied(path.to_string()));
        }

        // 206 Partial Content — server honored Range, read the partial body directly
        if status == 206 {
            let data = resp
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| BackendError::Internal(format!("Read: {e}")))?;
            return Ok(data);
        }

        // 416 Range Not Satisfiable — server doesn't support this range.
        // The response body is NOT file data (empty or error page).
        // Drop it and retry without Range header.
        if status == 416 {
            super::common::drain_response(resp).await;
            return self.http.read_full_and_slice(&url, offset, size).await;
        }

        if !resp.status().is_success() {
            super::common::drain_response(resp).await;
            return Err(BackendError::Internal(format!("GET failed: {status}")));
        }

        // Server returned 200 (full body) — stream past [0, offset) then read [offset, offset+size)
        // Guard: refuse to download enormous files just for a small range
        const MAX_FALLBACK_SIZE: u64 = 64 * 1024 * 1024; // 64 MB
        if let Some(len) = resp.content_length() {
            if len > MAX_FALLBACK_SIZE {
                super::common::drain_response(resp).await;
                return Err(BackendError::NotSupported(
                    "Server does not support Range requests for this file".into(),
                ));
            }
        }

        let need = size as usize;
        let skip = match usize::try_from(offset) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
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
                    continue;
                }
                // Partial skip: take the tail of this chunk
                let useful_start = skip_remaining;
                let useful = &chunk[useful_start..];
                let take = useful.len().min(need - buf.len());
                buf.extend_from_slice(&useful[..take]);
                skipped += useful_start;
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

        Ok(buf)
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

        let status = resp.status();
        let code = status.as_u16();
        let err = if code == 401 || code == 403 || code == 404 {
            super::common::check_http_status(code, path).err()
        } else if code == 413 {
            Some(BackendError::NotSupported(
                "File too large for server".to_string(),
            ))
        } else if !status.is_success() && code != 201 && code != 204 {
            Some(BackendError::Internal(format!("PUT failed: {status}")))
        } else {
            None
        };
        super::common::drain_response(resp).await;
        if let Some(e) = err {
            return Err(e);
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

        let status = resp.status();
        let code = status.as_u16();
        super::common::drain_response(resp).await;
        super::common::check_http_status(code, path)?;
        if code == 405 {
            return Err(BackendError::InvalidPath(format!(
                "Directory already exists: {path}"
            )));
        }
        if !status.is_success() && code != 201 {
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

        let status = resp.status();
        super::common::drain_response(resp).await;
        super::common::check_http_status(status.as_u16(), path)?;
        if !status.is_success() && status.as_u16() != 204 {
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

        let status = resp.status();
        super::common::drain_response(resp).await;
        super::common::check_http_status(status.as_u16(), from)?;
        if !status.is_success() && status.as_u16() != 201 && status.as_u16() != 204 {
            return Err(BackendError::Internal(format!("MOVE failed: {status}")));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_simple() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let url = backend.build_url("/file.txt").unwrap();
        assert_eq!(url, "http://host:5000/file.txt");
    }

    #[test]
    fn test_url_unicode() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let url = backend.build_url("/文件.txt").unwrap();
        assert_eq!(url, "http://host:5000/%E6%96%87%E4%BB%B6.txt");
    }

    #[test]
    fn test_url_trailing_slash() {
        let backend = WebDavBackend::new("http://host:5000/", false).unwrap();
        let url = backend.build_url("/file").unwrap();
        assert_eq!(url, "http://host:5000/file");
    }

    #[test]
    fn test_url_double_slash() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let url = backend.build_url("//file").unwrap();
        assert_eq!(url, "http://host:5000/file");
    }

    #[test]
    fn test_url_traversal_rejected() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let result = backend.build_url("/../../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_url_dotdot_in_filename_allowed() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let url = backend.build_url("/..hidden").unwrap();
        assert_eq!(url, "http://host:5000/..hidden");
    }

    #[test]
    fn test_url_null_byte_rejected() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        let result = backend.build_url("/file\x00.txt");
        assert!(result.is_err());
    }

    #[test]
    fn test_url_space() {
        let backend = WebDavBackend::new("http://host", false).unwrap();
        let url = backend.build_url("/my file.txt").unwrap();
        assert_eq!(url, "http://host/my%20file.txt");
    }

    #[test]
    fn test_url_hash() {
        let backend = WebDavBackend::new("http://host", false).unwrap();
        let url = backend.build_url("/file#section").unwrap();
        assert_eq!(url, "http://host/file%23section");
    }

    #[test]
    fn test_url_base_with_path_prefix() {
        // This is the exact pattern causing ghost directories:
        // base_url = http://host:5244/dav/ (trailing slash with path prefix)
        // path = /123pan
        let backend = WebDavBackend::new("http://host:5244/dav/", false).unwrap();
        let url = backend.build_url("/123pan").unwrap();
        assert_eq!(
            url, "http://host:5244/dav/123pan",
            "build_url must not produce double slash"
        );

        let url2 = backend.build_url("/123pan/subdir").unwrap();
        assert_eq!(url2, "http://host:5244/dav/123pan/subdir");

        let url_root = backend.build_url("/").unwrap();
        assert_eq!(url_root, "http://host:5244/dav/");
    }

    #[test]
    fn test_backend_new_basic() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        assert_eq!(backend.http.base_url.as_str(), "http://host:5000/");
    }

    #[test]
    fn test_backend_new_readonly() {
        let backend = WebDavBackend::new("http://host", true).unwrap();
        assert!(backend.is_read_only());
    }

    #[test]
    fn test_backend_protocol() {
        let backend = WebDavBackend::new("http://host:5000", false).unwrap();
        assert_eq!(backend.protocol(), "webdav");
    }

    #[test]
    fn test_backend_server_addr() {
        let backend = WebDavBackend::new("http://1.2.3.4:5000", false).unwrap();
        assert_eq!(backend.server_addr(), "http://1.2.3.4:5000/");
    }

    #[test]
    fn test_parse_single_file() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/hello.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>14</D:getcontentlength>
        <D:getlastmodified>Fri, 30 May 2026 10:00:00 GMT</D:getlastmodified>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");
        assert!(!entries[0].dir);
        assert_eq!(entries[0].size, 14);
    }

    #[test]
    fn test_parse_directory() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/docs/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "docs");
        assert!(entries[0].dir);
    }

    #[test]
    fn test_parse_stat_directory_no_filter() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/docs/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/docs/", false).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "docs");
        assert!(entries[0].dir);
    }

    #[test]
    fn test_parse_multiple() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/file1.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>100</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/file2.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>200</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/dir/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_parse_empty_dir() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/empty/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/empty/", true).unwrap();
        assert_eq!(entries.len(), 0);
    }

    #[test]
    fn test_parse_no_mtime() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/file.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>50</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mtime, UNIX_EPOCH);
    }

    #[test]
    fn test_parse_relative_href() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>file.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>10</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
    }

    #[test]
    fn test_parse_absolute_href() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/path/to/file.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>10</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
    }

    #[test]
    fn test_parse_filter_self_with_subpath_base_url() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/p1/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/p1/docs/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/></D:resourcetype>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/p1/file.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>100</D:getcontentlength>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;

        let entries_old = parse_propfind(xml, "/", true).unwrap();
        assert_eq!(
            entries_old.len(),
            3,
            "old behavior: self-entry not filtered"
        );

        let entries_new = parse_propfind(xml, "/p1/", true).unwrap();
        assert_eq!(entries_new.len(), 2, "self-entry /p1/ filtered out");
        assert_eq!(entries_new[0].name, "docs");
        assert!(entries_new[0].dir);
        assert_eq!(entries_new[1].name, "file.txt");
        assert!(!entries_new[1].dir);
    }
}
