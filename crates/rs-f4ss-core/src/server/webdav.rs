//! WebDAV protocol handlers: PROPFIND, PROPPATCH, LOCK, COPY.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::{format_http_date, FileServerState};

// ---------------------------------------------------------------------------
// PROPFIND
// ---------------------------------------------------------------------------

pub async fn handle_propfind(
    state: &Arc<FileServerState>,
    local_path: &Path,
    is_dir: bool,
    is_file: bool,
    size: u64,
    headers: &HeaderMap,
    url_path: &str,
) -> Response {
    if !is_dir && !is_file {
        return StatusCode::NOT_FOUND.into_response();
    }

    let depth = parse_depth(headers);

    if is_file {
        let mtime = file_mtime_sys(local_path).await;
        let name = extract_name(url_path);
        let xml = file_propfind_xml(url_path, &name, size, mtime);
        return multistatus_response(&xml);
    }

    // Directory
    let mtime = file_mtime_sys(local_path).await;
    let name = extract_name(url_path);
    let mut xml = dir_propfind_xml(url_path, &name, mtime);

    if depth == 1 {
        match state.list_dir(local_path).await {
            Ok(entries) => {
                for entry in &entries {
                    let href = if entry.is_dir {
                        format!("{}/{}/", url_path.trim_end_matches('/'), entry.name)
                    } else {
                        format!("{}/{}", url_path.trim_end_matches('/'), entry.name)
                    };
                    if entry.is_dir {
                        xml.push_str(&dir_propfind_xml(&href, &entry.name, entry.mtime));
                    } else {
                        xml.push_str(&file_propfind_xml(
                            &href,
                            &entry.name,
                            entry.size,
                            entry.mtime,
                        ));
                    }
                }
            }
            Err(e) => {
                tracing::error!("list_dir for PROPFIND: {e}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }

    multistatus_response(&xml)
}

// ---------------------------------------------------------------------------
// PROPPATCH (stub — returns 403 for all properties)
// ---------------------------------------------------------------------------

pub fn handle_proppatch(url_path: &str) -> Response {
    let href = xml_escape(url_path);
    let xml = format!(
        r#"<D:response>
<D:href>{href}</D:href>
<D:propstat>
<D:prop>
</D:prop>
<D:status>HTTP/1.1 403 Forbidden</D:status>
</D:propstat>
</D:response>"#
    );
    multistatus_response(&xml)
}

// ---------------------------------------------------------------------------
// LOCK (pseudo-lock)
// ---------------------------------------------------------------------------

pub fn handle_lock(url_path: &str, has_auth: bool) -> Response {
    let token = if has_auth {
        // Generate a pseudo-unique token from timestamp + path hash
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        url_path.hash(&mut hasher);
        std::time::SystemTime::now().hash(&mut hasher);
        format!("opaquelocktoken:{:016x}", hasher.finish())
    } else {
        format!("{}", chrono::Utc::now().timestamp())
    };

    let href = xml_escape(url_path);
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock>
<D:locktoken><D:href>{token}</D:href></D:locktoken>
<D:lockroot><D:href>{href}</D:href></D:lockroot>
</D:activelock></D:lockdiscovery></D:prop>"#
    );

    (
        StatusCode::OK,
        [("content-type", "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// XML builders
// ---------------------------------------------------------------------------

fn file_propfind_xml(href: &str, name: &str, size: u64, mtime: SystemTime) -> String {
    let displayname = xml_escape(name);
    let href = xml_escape(href);
    let mtime_str = format_http_date(mtime);
    format!(
        r#"<D:response>
<D:href>{href}</D:href>
<D:propstat>
<D:prop>
<D:displayname>{displayname}</D:displayname>
<D:getcontentlength>{size}</D:getcontentlength>
<D:getlastmodified>{mtime_str}</D:getlastmodified>
<D:resourcetype></D:resourcetype>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>"#
    )
}

fn dir_propfind_xml(href: &str, name: &str, mtime: SystemTime) -> String {
    let displayname = xml_escape(name);
    let mtime_str = format_http_date(mtime);
    let href_escaped = xml_escape(
        if href.ends_with('/') {
            href.to_string()
        } else {
            format!("{href}/")
        }
        .as_str(),
    );
    format!(
        r#"<D:response>
<D:href>{href_escaped}</D:href>
<D:propstat>
<D:prop>
<D:displayname>{displayname}</D:displayname>
<D:getlastmodified>{mtime_str}</D:getlastmodified>
<D:resourcetype><D:collection/></D:resourcetype>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>"#
    )
}

fn multistatus_response(content: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<D:multistatus xmlns:D="DAV:">
{content}
</D:multistatus>"#
    );
    (
        StatusCode::MULTI_STATUS,
        [("content-type", "application/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_depth(headers: &HeaderMap) -> u32 {
    headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            if v == "0" {
                Some(0)
            } else if v == "1" {
                Some(1)
            } else {
                None // infinity — reject, treat as 1
            }
        })
        .unwrap_or(1)
}

async fn file_mtime_sys(path: &Path) -> SystemTime {
    tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn extract_name(url_path: &str) -> String {
    let p = url_path.trim_end_matches('/');
    p.rsplit('/').next().unwrap_or(p).to_string()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
