//! HTTP + WebDAV file sharing server.
//!
//! Serves a local directory over HTTP (autoindex) and WebDAV (PROPFIND).
//! Output formats are compatible with rs-f4ss's own `HttpBackend` and
//! `WebDavBackend` clients, enabling peer-to-peer file sharing.

mod autoindex;
mod handlers;
mod viewer;
mod webdav;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use base64::{engine::general_purpose::STANDARD, Engine};
use tokio::io::AsyncWriteExt;
use tower_http::trace::TraceLayer;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Configuration for the file sharing server.
pub struct FileServerConfig {
    /// Local directory to serve.
    pub root: PathBuf,
    /// Read-only mode (reject PUT/DELETE/MOVE/MKCOL).
    pub read_only: bool,
    /// Optional Basic Auth (username, password).
    pub auth: Option<(String, String)>,
}

/// Shared server state, passed to all handlers via axum State.
pub struct FileServerState {
    root: PathBuf,
    read_only: bool,
    auth: Option<(String, String)>,
}

/// File/directory metadata for listing.
pub(crate) struct EntryMeta {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build an axum Router for the file sharing server.
pub fn create_router(config: FileServerConfig) -> (Router, Arc<FileServerState>) {
    let root = config.root.canonicalize().expect("Root path must exist");
    assert!(root.is_dir(), "Root path must be a directory");

    let state = Arc::new(FileServerState {
        root,
        read_only: config.read_only,
        auth: config.auth,
    });

    // No CORS layer: the share server is intended for same-origin browser visits
    // and direct HTTP/WebDAV clients. `CorsLayer::permissive()` short-circuits
    // OPTIONS preflight and drops the handler's Allow/DAV headers, breaking WebDAV
    // capability discovery. If cross-origin browser access is needed, place a
    // reverse proxy (nginx / caddy) in front.
    let router = Router::new()
        .fallback(handle_request)
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    (router, state)
}

/// Run the file sharing server (blocking).
pub async fn serve(config: FileServerConfig, addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (router, _state) = create_router(config);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("File server listening on {addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Request dispatcher
// ---------------------------------------------------------------------------

async fn handle_request(State(state): State<Arc<FileServerState>>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();
    let url_path = uri.path().to_string();
    let query = uri.query().map(|s| s.to_string());
    let body = req.into_body();

    // Auth check
    if let Err(status) = state.check_auth(&headers) {
        return (status, [("WWW-Authenticate", "Basic realm=\"rs-f4ss\"")]).into_response();
    }

    // Path safety: resolve and validate
    let local_path = match state.resolve_path(&url_path) {
        Ok(p) => p,
        Err(status) => return status.into_response(),
    };

    // Stat the path
    let meta = match tokio::fs::symlink_metadata(&local_path).await {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::error!("stat error for {}: {e}", local_path.display());
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
    let is_file = meta.as_ref().is_some_and(|m| m.is_file());
    let size = meta.as_ref().map_or(0, |m| m.len());

    // Directory redirect: /dir -> /dir/
    if is_dir && !url_path.ends_with('/') {
        return (
            StatusCode::MOVED_PERMANENTLY,
            [("Location", format!("{url_path}/"))],
        )
            .into_response();
    }

    let info = handlers::FileInfo {
        is_dir,
        is_file,
        size,
    };
    match method {
        Method::GET | Method::HEAD => {
            let head_only = method == Method::HEAD;
            handlers::handle_get(
                &state,
                &local_path,
                &info,
                &headers,
                &url_path,
                query.as_deref(),
                head_only,
            )
            .await
        }
        Method::PUT => handlers::handle_put(&state, &local_path, body).await,
        Method::DELETE => handlers::handle_delete(&state, &local_path, is_dir, is_file).await,
        Method::OPTIONS => handlers::handle_options(),
        _ => {
            let method_str = method.as_str();
            match method_str {
                "MKCOL" => handlers::handle_mkcol(&state, &local_path, meta.is_some()).await,
                "MOVE" => handlers::handle_move(&state, &local_path, &headers).await,
                "COPY" => handlers::handle_copy(&state, &local_path, &headers).await,
                "PROPFIND" => {
                    webdav::handle_propfind(
                        &state,
                        &local_path,
                        is_dir,
                        is_file,
                        size,
                        &headers,
                        &url_path,
                    )
                    .await
                }
                "PROPPATCH" => webdav::handle_proppatch(&url_path),
                "LOCK" => webdav::handle_lock(&url_path, headers.get("authorization").is_some()),
                "UNLOCK" => StatusCode::NO_CONTENT.into_response(),
                _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FileServerState methods
// ---------------------------------------------------------------------------

impl FileServerState {
    /// Resolve a URL path to a local filesystem path.
    /// Rejects path traversal (..), null bytes, symlink escape, and paths outside root.
    fn resolve_path(&self, url_path: &str) -> Result<PathBuf, StatusCode> {
        if url_path.contains('\0') {
            return Err(StatusCode::BAD_REQUEST);
        }

        let decoded = match percent_encoding::percent_decode_str(url_path).decode_utf8() {
            Ok(d) => d,
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        };

        let mut components = Vec::new();
        for comp in Path::new(&*decoded).components() {
            match comp {
                std::path::Component::Normal(c) => components.push(c),
                std::path::Component::CurDir | std::path::Component::RootDir => {}
                std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
        }

        let local = if components.is_empty() {
            self.root.clone()
        } else {
            let mut p = self.root.clone();
            for c in &components {
                p = p.join(c);
            }
            p
        };

        // Canonicalize to resolve symlinks and verify the path stays within root.
        // Path may not exist yet (e.g. PUT to new file) — that's OK, parent must exist.
        let canonical = match local.canonicalize() {
            Ok(c) => c,
            Err(_) if !local.exists() => match local.parent().and_then(|p| p.canonicalize().ok()) {
                Some(parent) if parent.starts_with(&self.root) => return Ok(local),
                _ => return Err(StatusCode::BAD_REQUEST),
            },
            Err(_) => return Err(StatusCode::NOT_FOUND),
        };

        if !canonical.starts_with(&self.root) {
            return Err(StatusCode::BAD_REQUEST);
        }

        Ok(canonical)
    }

    /// Check Basic Auth.
    fn check_auth(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        let Some((expected_user, expected_hash)) = &self.auth else {
            return Ok(());
        };

        let auth_header = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                let prefix = v.get(..5.min(v.len()))?.to_ascii_lowercase();
                if prefix == "basic" {
                    v.get(6..)
                } else {
                    None
                }
            });

        let Some(encoded) = auth_header else {
            return Err(StatusCode::UNAUTHORIZED);
        };

        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let credentials = String::from_utf8(decoded).map_err(|_| StatusCode::UNAUTHORIZED)?;

        match credentials.split_once(':') {
            Some((user, pass)) => {
                if user == expected_user {
                    let incoming_hash = crate::persistence::sha256_hex(pass);
                    if incoming_hash == *expected_hash {
                        return Ok(());
                    }
                }
                Err(StatusCode::UNAUTHORIZED)
            }
            _ => Err(StatusCode::UNAUTHORIZED),
        }
    }

    /// List directory entries with metadata.
    pub(crate) async fn list_dir(&self, dir: &Path) -> std::io::Result<Vec<EntryMeta>> {
        let mut entries = Vec::new();
        let mut rd = tokio::fs::read_dir(dir).await?;

        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }

            let meta = match entry.metadata().await {
                Ok(m) => m,
                Err(_) => continue,
            };

            if !meta.is_file() && !meta.is_dir() {
                continue;
            }

            entries.push(EntryMeta {
                name,
                is_dir: meta.is_dir(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }

        // Directories first, then alphabetical
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// Shared utilities
// ---------------------------------------------------------------------------

pub(crate) const BUF_SIZE: usize = 262144; // 256KB
pub(crate) const MAX_UPLOAD_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2GB

pub(crate) fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("rar") => "application/x-rar-compressed",
        Some("7z") => "application/x-7z-compressed",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Parse Range header. Returns (start, end) inclusive, or None if invalid.
pub(crate) fn parse_range(range_header: &str, file_size: u64) -> Option<(u64, u64)> {
    let range = range_header.strip_prefix("bytes=")?;
    let range = range.trim();

    if let Some(rest) = range.strip_prefix('-') {
        let suffix: u64 = rest.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = file_size.saturating_sub(suffix);
        return Some((start, file_size.saturating_sub(1)));
    }

    let (start_str, rest) = range.split_once('-')?;
    let start: u64 = start_str.parse().ok()?;
    let end = if rest.is_empty() {
        file_size.saturating_sub(1)
    } else {
        let e: u64 = rest.parse().ok()?;
        e.min(file_size.saturating_sub(1))
    };

    if start > end {
        return None;
    }

    Some((start, end))
}

pub(crate) fn format_http_date(t: SystemTime) -> String {
    let dur = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    chrono::DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
        .map(|dt| dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
        .unwrap_or_default()
}

pub(crate) fn file_to_body(file: tokio::fs::File) -> axum::body::Body {
    let stream = tokio_util::io::ReaderStream::with_capacity(file, BUF_SIZE);
    axum::body::Body::from_stream(stream)
}

pub(crate) async fn write_body_to_file(
    body: axum::body::Body,
    path: &Path,
) -> Result<StatusCode, Response> {
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir: {e}")).into_response());
        }
    }

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("create: {e}")).into_response())?;

    let mut stream = body.into_data_stream();
    let mut total: u64 = 0;

    use futures_util::TryStreamExt;

    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("read body: {e}")).into_response())?
    {
        total += chunk.len() as u64;
        if total > MAX_UPLOAD_SIZE {
            let _ = tokio::fs::remove_file(path).await;
            return Err(StatusCode::PAYLOAD_TOO_LARGE.into_response());
        }
        file.write_all(&chunk).await.map_err(|e| {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("write: {e}")).into_response()
        })?;
    }

    file.flush()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("flush: {e}")).into_response())?;

    let status = if total == 0 {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CREATED
    };
    Ok(status)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_path tests ---

    fn make_state_with_tmp() -> (FileServerState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = FileServerState {
            root: tmp.path().canonicalize().unwrap(),
            read_only: false,
            auth: None,
        };
        (state, tmp)
    }

    #[test]
    fn test_resolve_path_normal() {
        let (state, _tmp) = make_state_with_tmp();
        let p = state.resolve_path("/").unwrap();
        assert_eq!(p, state.root);
    }

    #[test]
    fn test_resolve_path_subdir() {
        let (state, _tmp) = make_state_with_tmp();
        // Create subdirectories so canonicalize succeeds
        std::fs::create_dir_all(state.root.join("a/b/c")).unwrap();
        let p = state.resolve_path("/a/b/c").unwrap();
        assert_eq!(p, state.root.join("a/b/c"));
    }

    #[test]
    fn test_resolve_path_new_file() {
        let (state, _tmp) = make_state_with_tmp();
        // New file in existing directory should succeed
        let p = state.resolve_path("/newfile.txt").unwrap();
        assert_eq!(p, state.root.join("newfile.txt"));
    }

    #[test]
    fn test_resolve_path_reject_traversal() {
        let (state, _tmp) = make_state_with_tmp();
        assert!(state.resolve_path("/../etc/passwd").is_err());
        assert!(state.resolve_path("/a/../../etc/passwd").is_err());
    }

    #[test]
    fn test_resolve_path_reject_null() {
        let (state, _tmp) = make_state_with_tmp();
        assert!(state.resolve_path("/file\x00.txt").is_err());
    }

    #[test]
    fn test_resolve_path_root() {
        let (state, _tmp) = make_state_with_tmp();
        let p = state.resolve_path("/").unwrap();
        assert_eq!(p, state.root);
    }

    // --- parse_range tests ---

    #[test]
    fn test_range_normal() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
    }

    #[test]
    fn test_range_open_end() {
        assert_eq!(parse_range("bytes=0-", 1000), Some((0, 999)));
    }

    #[test]
    fn test_range_suffix() {
        assert_eq!(parse_range("bytes=-300", 1000), Some((700, 999)));
    }

    #[test]
    fn test_range_suffix_larger_than_file() {
        assert_eq!(parse_range("bytes=-2000", 1000), Some((0, 999)));
    }

    #[test]
    fn test_range_start_beyond_size() {
        assert_eq!(parse_range("bytes=1000-", 1000), None);
    }

    #[test]
    fn test_range_invalid_format() {
        assert_eq!(parse_range("bytes=", 1000), None);
        assert_eq!(parse_range("invalid", 1000), None);
    }

    #[test]
    fn test_range_suffix_zero() {
        assert_eq!(parse_range("bytes=-0", 1000), None);
    }

    // --- content_type_for tests ---

    #[test]
    fn test_content_type() {
        assert_eq!(
            content_type_for(Path::new("file.txt")),
            "text/plain; charset=utf-8"
        );
        assert_eq!(content_type_for(Path::new("image.png")), "image/png");
        assert_eq!(
            content_type_for(Path::new("data.json")),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("unknown.xyz")),
            "application/octet-stream"
        );
    }

    // --- check_auth tests ---

    fn make_auth_state() -> FileServerState {
        FileServerState {
            root: PathBuf::from("/tmp"),
            read_only: false,
            auth: None,
        }
    }

    #[test]
    fn test_auth_no_config() {
        let state = make_auth_state();
        assert!(state.check_auth(&HeaderMap::new()).is_ok());
    }

    #[test]
    fn test_auth_valid() {
        let mut state = make_auth_state();
        state.auth = Some((
            "admin".to_string(),
            crate::persistence::sha256_hex("secret"),
        ));
        let mut headers = HeaderMap::new();
        let creds = STANDARD.encode("admin:secret");
        headers.insert("authorization", format!("Basic {creds}").parse().unwrap());
        assert!(state.check_auth(&headers).is_ok());
    }

    #[test]
    fn test_auth_invalid() {
        let mut state = make_auth_state();
        state.auth = Some((
            "admin".to_string(),
            crate::persistence::sha256_hex("secret"),
        ));
        let mut headers = HeaderMap::new();
        let creds = STANDARD.encode("admin:wrong");
        headers.insert("authorization", format!("Basic {creds}").parse().unwrap());
        assert!(state.check_auth(&headers).is_err());
    }

    #[test]
    fn test_auth_missing_header() {
        let mut state = make_auth_state();
        state.auth = Some((
            "admin".to_string(),
            crate::persistence::sha256_hex("secret"),
        ));
        assert!(state.check_auth(&HeaderMap::new()).is_err());
    }
}
