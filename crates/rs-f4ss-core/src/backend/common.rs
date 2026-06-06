//! Shared HTTP client infrastructure for backend implementations.

use base64::Engine;
use bytes::Bytes;
use reqwest::Client;
use std::time::Duration;
use url::Url;

use crate::error::BackendError;

/// Drain the response body to allow connection reuse. Silently ignores errors.
pub(crate) async fn drain_response(resp: reqwest::Response) {
    let _ = resp.bytes().await;
}

pub(crate) fn should_retry_request(method: &reqwest::Method, _has_body: bool) -> bool {
    matches!(method.as_str(), "GET" | "HEAD" | "PROPFIND")
}

pub(crate) struct HttpClient {
    pub(crate) base_url: Url,
    pub(crate) client: Client,
    pub(crate) auth_header: Option<String>,
}

impl HttpClient {
    pub(crate) fn new(
        base_url: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, BackendError> {
        let url = Url::parse(base_url)
            .map_err(|e| BackendError::InvalidPath(format!("Invalid URL: {e}")))?;

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(120))
            .pool_max_idle_per_host(8)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| BackendError::Internal(format!("HTTP client: {e}")))?;

        let auth_header = build_auth_header(username, password)?;

        Ok(Self {
            base_url: url,
            client,
            auth_header,
        })
    }

    pub(crate) fn build_url(&self, path: &str) -> Result<String, BackendError> {
        if path.split('/').any(|c| c == ".." || c == ".") {
            return Err(BackendError::InvalidPath(
                "Path traversal not allowed".into(),
            ));
        }
        if path.contains('\0') {
            return Err(BackendError::InvalidPath("Null byte in path".into()));
        }
        // Reject percent-encoded path traversal (%2e%2e = ..)
        let lower = path.to_ascii_lowercase();
        if lower.contains("%2e%2e") || lower.contains("%2e.") || lower.contains(".%2e") {
            return Err(BackendError::InvalidPath(
                "Encoded path traversal not allowed".into(),
            ));
        }
        let mut url = self.base_url.clone();
        let path = path.trim_start_matches('/');
        if !path.is_empty() {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| BackendError::InvalidPath("Cannot set path".into()))?;
            // Pop trailing empty segment left by base_url trailing slash (e.g. "/dav/" → ["dav",""])
            // to avoid double-slash when extending (e.g. "/dav//subdir")
            if self.base_url.path().ends_with('/')
                && self
                    .base_url
                    .path_segments()
                    .is_some_and(|mut s| s.next_back() == Some(""))
            {
                segments.pop();
            }
            segments.extend(path.split('/'));
        }
        let mut out = url.to_string();
        // Defense in depth: collapse any remaining "//" in the path portion
        if let Some(idx) = out.find("//") {
            let scheme_end = out.find("://").map_or(0, |i| i + 3);
            if idx >= scheme_end {
                // Only collapse // in the path, not in the scheme://
                let path_part = &out[scheme_end..];
                out = out[..scheme_end].to_string() + &path_part.replace("//", "/");
            }
        }
        Ok(out)
    }

    pub(crate) async fn send_with_retry(
        &self,
        method: reqwest::Method,
        url: &str,
        headers: Vec<(&str, String)>,
        body: Option<Bytes>,
    ) -> Result<reqwest::Response, BackendError> {
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0u32;
        // Only retry idempotent read operations. PUT/MKCOL/DELETE/MOVE are not
        // safe to retry because the server may have already processed the request
        // but failed to send the response.
        let is_retryable = should_retry_request(&method, body.is_some());

        loop {
            let mut req = self.client.request(method.clone(), url);
            req = req.timeout(Duration::from_secs(30));
            if let Some(ref auth) = self.auth_header {
                req = req.header("Authorization", auth.as_str());
            }
            for (key, value) in &headers {
                req = req.header(*key, value.as_str());
            }
            if let Some(ref data) = body {
                req = req.body(data.clone());
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if is_retryable
                        && matches!(status, 500 | 502 | 503 | 504)
                        && attempt < MAX_RETRIES
                    {
                        drain_response(resp).await;
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e)
                    if is_retryable
                        && (e.is_connect() || e.is_timeout())
                        && attempt < MAX_RETRIES =>
                {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                    continue;
                }
                Err(e) => return Err(BackendError::ConnectionFailed(e.to_string())),
            }
        }
    }

    pub(crate) async fn read_full_and_slice(
        &self,
        url: &str,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, BackendError> {
        // Uses send_with_retry which has 30s per-request timeout.
        // This is a fallback for small files only; large reads use the
        // backend's own read() with Range + 300s timeout.
        let resp = self
            .send_with_retry(reqwest::Method::GET, url, vec![], None)
            .await?;
        let status = resp.status().as_u16();
        if status == 404 {
            drain_response(resp).await;
            return Err(BackendError::NotFound(String::new()));
        }
        if status == 401 {
            drain_response(resp).await;
            return Err(BackendError::PermissionDenied(
                "Authentication required".into(),
            ));
        }
        if status == 403 {
            drain_response(resp).await;
            return Err(BackendError::PermissionDenied(url.to_string()));
        }
        if !resp.status().is_success() {
            drain_response(resp).await;
            return Err(BackendError::Internal(format!("GET failed: {status}")));
        }
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
        Ok(data[start..end].to_vec())
    }
}

pub(crate) fn build_auth_header(
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Option<String>, BackendError> {
    match (username, password) {
        (Some(u), Some(p)) => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
            Ok(Some(format!("Basic {encoded}")))
        }
        (Some(_), None) | (None, Some(_)) => Err(BackendError::InvalidPath(
            "Both username and password required".into(),
        )),
        _ => Ok(None),
    }
}

/// Safely convert a chrono `DateTime<Utc>` to `SystemTime`.
/// chrono's `From<DateTime<Tz>> for SystemTime` panics on Windows when the
/// timestamp is before 1970 because `SystemTime - Duration` overflows
/// (Windows SystemTime starts at 1601-01-01).
/// Check common HTTP status codes and return appropriate BackendError.
/// Returns Ok(()) if status is not a recognized error.
pub(crate) fn check_http_status(status: u16, path: &str) -> Result<(), BackendError> {
    match status {
        401 => Err(BackendError::PermissionDenied(
            "Authentication required".into(),
        )),
        403 => Err(BackendError::PermissionDenied(path.to_string())),
        404 => Err(BackendError::NotFound(path.to_string())),
        _ => Ok(()),
    }
}

#[cfg(any(feature = "webdav", feature = "http", feature = "s3"))]
pub(crate) fn datetime_to_systemtime(dt: &chrono::DateTime<chrono::Utc>) -> std::time::SystemTime {
    let sec = dt.timestamp();
    let nsec = dt.timestamp_subsec_nanos();
    if sec >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::new(sec as u64, nsec)
    } else {
        let neg_sec = (-sec) as u64;
        std::time::UNIX_EPOCH
            .checked_sub(std::time::Duration::new(neg_sec, nsec))
            .unwrap_or(std::time::UNIX_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    fn read_http_request(stream: &mut std::net::TcpStream) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                return Ok(buf);
            }
        }
        Ok(buf)
    }

    #[test]
    fn test_retry_policy_allows_get_without_body() {
        assert!(should_retry_request(&reqwest::Method::GET, false));
    }

    #[test]
    fn test_retry_policy_rejects_put_even_without_body() {
        assert!(!should_retry_request(&reqwest::Method::PUT, false));
    }

    #[test]
    fn test_retry_policy_rejects_webdav_move_without_body() {
        let move_method = reqwest::Method::from_bytes(b"MOVE").unwrap();
        assert!(!should_retry_request(&move_method, false));
    }

    #[tokio::test]
    async fn test_read_full_error_drains_body_for_connection_reuse() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();

            let _ = read_http_request(&mut stream).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 5\r\nConnection: keep-alive\r\n\r\nerror",
                )
                .unwrap();

            let reused = read_http_request(&mut stream)
                .map(|req| !req.is_empty())
                .unwrap_or(false);

            if reused {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .unwrap();
                tx.send(1usize).unwrap();
                return;
            }

            let (mut stream2, _) = listener.accept().unwrap();
            let _ = read_http_request(&mut stream2).unwrap();
            stream2
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            tx.send(2usize).unwrap();
        });

        let http = HttpClient::new(&format!("http://{addr}/"), None, None).unwrap();
        let url = http.build_url("/test.txt").unwrap();

        let err = http.read_full_and_slice(&url, 0, 4).await.unwrap_err();
        assert!(matches!(err, BackendError::PermissionDenied(_)));

        let resp = http
            .send_with_retry(reqwest::Method::GET, &url, vec![], None)
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    }
}
