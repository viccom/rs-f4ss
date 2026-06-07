//! HTTP handlers: GET/HEAD, PUT, DELETE, MOVE, MKCOL, COPY, OPTIONS.

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use super::autoindex;
use super::viewer;
use super::FileServerState;
use super::{
    content_type_for, dir_etag, file_to_body, forces_download, format_http_date, parse_range,
    write_body_to_file,
};

// ---------------------------------------------------------------------------
// GET / HEAD
// ---------------------------------------------------------------------------

pub(crate) struct FileInfo {
    pub(crate) is_dir: bool,
    pub(crate) is_file: bool,
    pub(crate) size: u64,
}

pub async fn handle_get(
    state: &Arc<FileServerState>,
    local_path: &Path,
    info: &FileInfo,
    headers: &HeaderMap,
    url_path: &str,
    query: Option<&str>,
    head_only: bool,
) -> Response {
    if !info.is_dir && !info.is_file {
        return StatusCode::NOT_FOUND.into_response();
    }

    if info.is_dir {
        return handle_get_dir(state, local_path, url_path, query, headers, head_only).await;
    }

    handle_get_file(local_path, info.size, headers, head_only).await
}

async fn handle_get_dir(
    state: &Arc<FileServerState>,
    local_path: &Path,
    url_path: &str,
    query: Option<&str>,
    headers: &HeaderMap,
    head_only: bool,
) -> Response {
    let entries = match state.list_dir(local_path).await {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("list_dir {}: {e}", local_path.display());
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Dispatch: browser visitor → embedded SPA; rs-f4ss / curl / etc → autoindex.
    let html = if viewer::wants_viewer(headers, query) {
        viewer::render_viewer_html(state, &entries, url_path)
    } else {
        autoindex::generate_autoindex(&entries, url_path)
    };

    let etag = dir_etag(&entries);
    // Honour If-None-Match: identical listing → 304, skip body serialisation.
    if let Some(inm) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) {
        if inm.split(',').any(|tag| tag.trim() == etag) {
            let mut hdrs = HeaderMap::new();
            hdrs.insert("etag", HeaderValue::from_str(&etag).unwrap());
            hdrs.insert("x-frame-options", HeaderValue::from_static("DENY"));
            hdrs.insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            return (StatusCode::NOT_MODIFIED, hdrs, Body::empty()).into_response();
        }
    }

    let body = if head_only {
        Body::empty()
    } else {
        Body::from(html)
    };

    let mut hdrs = HeaderMap::new();
    hdrs.insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    hdrs.insert("cache-control", HeaderValue::from_static("no-cache"));
    hdrs.insert("etag", HeaderValue::from_str(&etag).unwrap());
    // Clickjacking: the viewer exposes upload/delete buttons; deny embedding.
    hdrs.insert("x-frame-options", HeaderValue::from_static("DENY"));
    // Disable content sniffing for the HTML response (defence in depth).
    hdrs.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );

    (StatusCode::OK, hdrs, body).into_response()
}

async fn handle_get_file(
    local_path: &Path,
    size: u64,
    headers: &HeaderMap,
    head_only: bool,
) -> Response {
    let mtime = tokio::fs::metadata(local_path)
        .await
        .ok()
        .and_then(|m| m.modified().ok());

    let etag = mtime
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| format!("\"{}-{}\"", d.as_secs(), size));

    let last_modified = mtime.map(format_http_date);

    let range_header = headers.get("range").and_then(|v| v.to_str().ok());

    if let (Some(range_str), true) = (range_header, size > 0) {
        if let Some((start, end)) = parse_range(range_str, size) {
            return serve_range(
                local_path,
                size,
                start,
                end,
                &etag,
                &last_modified,
                head_only,
            )
            .await;
        }
        if range_str.starts_with("bytes=") {
            let mut hdrs = HeaderMap::new();
            hdrs.insert(
                "content-range",
                HeaderValue::from_str(&format!("bytes */{size}")).unwrap(),
            );
            return (StatusCode::RANGE_NOT_SATISFIABLE, hdrs, Body::empty()).into_response();
        }
    }

    let mut hdrs = HeaderMap::new();
    hdrs.insert(
        "content-type",
        HeaderValue::from_static(content_type_for(local_path)),
    );
    hdrs.insert(
        "content-length",
        HeaderValue::from_str(&size.to_string()).unwrap(),
    );
    hdrs.insert("accept-ranges", HeaderValue::from_static("bytes"));
    // nosniff stops browsers from second-guessing the MIME we declared.
    hdrs.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    // Force download for HTML/SVG to neuter stored-XSS in the share origin.
    if forces_download(local_path) {
        let name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        hdrs.insert(
            "content-disposition",
            HeaderValue::from_str(&format!("attachment; filename=\"{}\"", name)).unwrap(),
        );
    }

    if let Some(ref lm) = last_modified {
        hdrs.insert("last-modified", HeaderValue::from_str(lm).unwrap());
    }
    if let Some(ref et) = etag {
        hdrs.insert("etag", HeaderValue::from_str(et).unwrap());
    }

    if head_only {
        return (StatusCode::OK, hdrs, Body::empty()).into_response();
    }

    let file = match tokio::fs::File::open(local_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    (StatusCode::OK, hdrs, file_to_body(file)).into_response()
}

async fn serve_range(
    local_path: &Path,
    file_size: u64,
    start: u64,
    end: u64,
    etag: &Option<String>,
    last_modified: &Option<String>,
    head_only: bool,
) -> Response {
    let content_length = end - start + 1;
    let content_range = format!("bytes {start}-{end}/{file_size}");

    let mut hdrs = HeaderMap::new();
    hdrs.insert(
        "content-type",
        HeaderValue::from_static(content_type_for(local_path)),
    );
    hdrs.insert(
        "content-length",
        HeaderValue::from_str(&content_length.to_string()).unwrap(),
    );
    hdrs.insert(
        "content-range",
        HeaderValue::from_str(&content_range).unwrap(),
    );
    hdrs.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if forces_download(local_path) {
        let name = local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("download");
        hdrs.insert(
            "content-disposition",
            HeaderValue::from_str(&format!("attachment; filename=\"{}\"", name)).unwrap(),
        );
    }
    if let Some(ref lm) = last_modified {
        hdrs.insert("last-modified", HeaderValue::from_str(lm).unwrap());
    }
    if let Some(ref et) = etag {
        hdrs.insert("etag", HeaderValue::from_str(et).unwrap());
    }

    if head_only {
        return (StatusCode::PARTIAL_CONTENT, hdrs, Body::empty()).into_response();
    }

    let mut file = match tokio::fs::File::open(local_path).await {
        Ok(f) => f,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
    if file.seek(SeekFrom::Start(start)).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    let stream =
        tokio_util::io::ReaderStream::with_capacity(file.take(content_length), super::BUF_SIZE);
    let body = Body::from_stream(stream);

    (StatusCode::PARTIAL_CONTENT, hdrs, body).into_response()
}

// ---------------------------------------------------------------------------
// PUT
// ---------------------------------------------------------------------------

pub async fn handle_put(state: &Arc<FileServerState>, local_path: &Path, body: Body) -> Response {
    if state.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    match write_body_to_file(body, local_path).await {
        Ok(status) => status.into_response(),
        Err(response) => response,
    }
}

// ---------------------------------------------------------------------------
// DELETE
// ---------------------------------------------------------------------------

pub async fn handle_delete(
    state: &Arc<FileServerState>,
    local_path: &Path,
    is_dir: bool,
    is_file: bool,
) -> Response {
    if state.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    if !is_dir && !is_file {
        return StatusCode::NOT_FOUND.into_response();
    }

    let result = if is_dir {
        tokio::fs::remove_dir_all(local_path).await
    } else {
        tokio::fs::remove_file(local_path).await
    };

    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!("delete {}: {e}", local_path.display());
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// MKCOL
// ---------------------------------------------------------------------------

pub async fn handle_mkcol(
    state: &Arc<FileServerState>,
    local_path: &Path,
    exists: bool,
) -> Response {
    if state.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    if exists {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    match tokio::fs::create_dir_all(local_path).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => {
            tracing::error!("mkcol {}: {e}", local_path.display());
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// MOVE
// ---------------------------------------------------------------------------

pub async fn handle_move(
    state: &Arc<FileServerState>,
    local_path: &Path,
    headers: &HeaderMap,
) -> Response {
    if state.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    let dest = match extract_destination(state, headers) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let dest_exists = tokio::fs::symlink_metadata(&dest).await.is_ok();

    // Overwrite: F → reject if destination exists
    if dest_exists {
        let overwrite = headers
            .get("overwrite")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("T");
        if overwrite.eq_ignore_ascii_case("F") {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
    }

    match tokio::fs::rename(local_path, &dest).await {
        Ok(()) => {
            if dest_exists {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::CREATED.into_response()
            }
        }
        Err(e) => {
            tracing::error!("move: {e}");
            if e.raw_os_error() == Some(18) {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// COPY
// ---------------------------------------------------------------------------

pub async fn handle_copy(
    state: &Arc<FileServerState>,
    local_path: &Path,
    headers: &HeaderMap,
) -> Response {
    if state.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    let dest = match extract_destination(state, headers) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    let dest_exists = tokio::fs::symlink_metadata(&dest).await.is_ok();

    if dest_exists {
        let overwrite = headers
            .get("overwrite")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("T");
        if overwrite.eq_ignore_ascii_case("F") {
            return StatusCode::PRECONDITION_FAILED.into_response();
        }
    }

    let meta = match tokio::fs::symlink_metadata(local_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("copy stat: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let result = if meta.is_dir() {
        copy_dir_recursive(local_path, &dest).await
    } else {
        tokio::fs::copy(local_path, &dest).await.map(|_| ())
    };

    match result {
        Ok(()) => {
            if dest_exists {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::CREATED.into_response()
            }
        }
        Err(e) => {
            tracing::error!("copy: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// OPTIONS
// ---------------------------------------------------------------------------

pub fn handle_options() -> Response {
    let mut hdrs = HeaderMap::new();
    hdrs.insert(
        "allow",
        HeaderValue::from_static(
            "GET,HEAD,PUT,DELETE,OPTIONS,PROPFIND,MKCOL,MOVE,COPY,LOCK,UNLOCK",
        ),
    );
    hdrs.insert("dav", HeaderValue::from_static("1"));
    (StatusCode::NO_CONTENT, hdrs, Body::empty()).into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_destination(
    state: &FileServerState,
    headers: &HeaderMap,
) -> Result<std::path::PathBuf, Box<Response>> {
    let dest_url = headers
        .get("destination")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Box::new(StatusCode::BAD_REQUEST.into_response()))?;

    let path = if dest_url.starts_with("http://") || dest_url.starts_with("https://") {
        url::Url::parse(dest_url)
            .map(|u| u.path().to_string())
            .map_err(|_| Box::new(StatusCode::BAD_REQUEST.into_response()))?
    } else {
        dest_url.to_string()
    };

    state
        .resolve_path(&path)
        .map_err(|s| Box::new(s.into_response()))
}

fn copy_dir_recursive<'a>(
    src: &'a Path,
    dst: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>> + Send + 'a>> {
    Box::pin(async move {
        tokio::fs::create_dir_all(dst).await?;
        let mut entries = tokio::fs::read_dir(src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = entry.file_name();
            let dst_path = dst.join(&file_name);
            let meta = entry.metadata().await?;
            if meta.is_dir() {
                copy_dir_recursive(&src_path, &dst_path).await?;
            } else if meta.is_file() {
                tokio::fs::copy(&src_path, &dst_path).await?;
            }
        }
        Ok(())
    })
}
