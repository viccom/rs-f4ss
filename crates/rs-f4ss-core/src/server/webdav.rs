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

    let depth = match parse_depth(headers) {
        Ok(d) => d,
        Err(status) => return status.into_response(),
    };

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
// PROPPATCH — rejects all property modifications (read-only server).
// Lists standard DAV live properties so clients get a well-formed response.
// ---------------------------------------------------------------------------

pub fn handle_proppatch(url_path: &str, body: Option<&[u8]>) -> Response {
    let href = xml_escape(url_path);

    // Extract property element names from <D:set><D:prop>...</D:prop></D:set>
    // and <D:remove><D:prop>...</D:prop></D:remove> blocks.
    let prop_names = body
        .map(extract_prop_names)
        .unwrap_or_default();

    let prop_inner = if prop_names.is_empty() {
        // No parseable body — list standard DAV live properties.
        "\
\t<D:creationdate/>\n\
\t<D:displayname/>\n\
\t<D:getcontentlength/>\n\
\t<D:getcontenttype/>\n\
\t<D:getetag/>\n\
\t<D:getlastmodified/>\n\
\t<D:resourcetype/>"
            .to_string()
    } else {
        prop_names
            .iter()
            .map(|n| format!("\t<{n}/>"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let xml = format!(
        r#"<D:response>
<D:href>{href}</D:href>
<D:propstat>
<D:prop>
{prop_inner}
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
// UNLOCK
// ---------------------------------------------------------------------------

pub fn handle_unlock() -> Response {
    // DAV:1 pseudo-lock: always succeed. No token validation.
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// XML builders
// ---------------------------------------------------------------------------

fn file_propfind_xml(href: &str, name: &str, size: u64, mtime: SystemTime) -> String {
    let displayname = xml_escape(name);
    let href = xml_escape(href);
    let mtime_str = format_http_date(mtime);
    let content_type = super::content_type_for(Path::new(name));
    format!(
        r#"<D:response>
<D:href>{href}</D:href>
<D:propstat>
<D:prop>
<D:displayname>{displayname}</D:displayname>
<D:getcontentlength>{size}</D:getcontentlength>
<D:getcontenttype>{content_type}</D:getcontenttype>
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

fn parse_depth(headers: &HeaderMap) -> Result<u32, StatusCode> {
    headers
        .get("depth")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            if v == "0" {
                Some(0)
            } else if v == "1" {
                Some(1)
            } else if v == "infinity" || v == "infinite" {
                None
            } else {
                Some(1) // default
            }
        })
        .map(Ok)
        .unwrap_or(Err(StatusCode::FORBIDDEN))
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

/// Extract property element names from PROPPATCH XML body.
///
/// Looks inside `<D:set><D:prop>` and `<D:remove><D:prop>` blocks and
/// collects the tag names of direct children (the properties being set/removed).
/// Uses simple text scanning — no full XML parser needed for this read-only stub.
fn extract_prop_names(body: &[u8]) -> Vec<String> {
    let text = match std::str::from_utf8(body) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let mut names = Vec::new();

    // Find all <D:prop>...</D:prop> blocks (inside <D:set> or <D:remove>)
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find("<D:prop") {
        let abs_start = search_from + start;
        // Skip to end of opening tag
        let tag_end = text[abs_start..].find('>').map(|i| abs_start + i + 1);
        let Some(tag_end) = tag_end else { break };
        let close = text[tag_end..].find("</D:prop>").map(|i| tag_end + i);
        let Some(close) = close else {
            search_from = abs_start + 6;
            continue;
        };

        let inner = &text[tag_end..close];
        // Extract tag names of direct children: <prefix:name ...> or <name ...>
        let mut pos = 0;
        while let Some(lt) = inner[pos..].find('<') {
            let abs = pos + lt;
            if inner[abs..].starts_with("</") || inner[abs..].starts_with("<!--") {
                pos = abs + 2;
                continue;
            }
            let tag_content_start = abs + 1;
            let tag_content_end = inner[tag_content_start..]
                .find(|c: char| "> \n\r\t/".contains(c))
                .map(|i| tag_content_start + i)
                .unwrap_or(inner.len());
            let tag_name = inner[tag_content_start..tag_content_end].trim();
            if !tag_name.is_empty()
                && tag_name != "D:prop"
                && tag_name != "D:set"
                && tag_name != "D:remove"
            {
                names.push(tag_name.to_string());
            }
            pos = abs + 1;
        }

        search_from = close + 9; // skip past </D:prop>
    }

    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_prop_names_set_and_remove() {
        let body = br#"<?xml version="1.0"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set>
    <D:prop>
      <D:displayname>New Name</D:displayname>
      <D:getcontenttype>text/plain</D:getcontenttype>
    </D:prop>
  </D:set>
  <D:remove>
    <D:prop>
      <D:creationdate/>
      <D:comment/>
    </D:prop>
  </D:remove>
</D:propertyupdate>"#;
        let names = extract_prop_names(body);
        assert_eq!(names, vec!["D:displayname", "D:getcontenttype", "D:creationdate", "D:comment"]);
    }

    #[test]
    fn test_extract_prop_names_empty_body() {
        let names = extract_prop_names(b"");
        assert!(names.is_empty());
    }

    #[test]
    fn test_extract_prop_names_no_prop_blocks() {
        let names = extract_prop_names(b"<html>random stuff</html>");
        assert!(names.is_empty());
    }

    #[test]
    fn test_proppatch_with_body() {
        let body = br#"<?xml version="1.0"?>
<D:propertyupdate xmlns:D="DAV:">
  <D:set><D:prop><D:displayname>Hello</D:displayname></D:prop></D:set>
</D:propertyupdate>"#;
        let resp = handle_proppatch("/foo.txt", Some(body));
        let (status, headers, body_bytes) = decompose_response(resp);
        assert_eq!(status, StatusCode::MULTI_STATUS);
        assert_eq!(headers.get("content-type").unwrap(), "application/xml; charset=utf-8");
        let xml = String::from_utf8(body_bytes).unwrap();
        assert!(xml.contains("<D:displayname/>"), "should echo the requested property");
        assert!(xml.contains("403 Forbidden"));
    }

    #[test]
    fn test_proppatch_without_body_falls_back_to_defaults() {
        let resp = handle_proppatch("/foo.txt", None);
        let (_, _, body_bytes) = decompose_response(resp);
        let xml = String::from_utf8(body_bytes).unwrap();
        assert!(xml.contains("<D:displayname/>"));
        assert!(xml.contains("<D:getlastmodified/>"));
        assert!(xml.contains("403 Forbidden"));
    }

    fn decompose_response(resp: Response) -> (StatusCode, HeaderMap, Vec<u8>) {
        let (parts, body) = resp.into_parts();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let bytes = rt.block_on(axum::body::to_bytes(body, 1_000_000)).unwrap().to_vec();
        (parts.status, parts.headers, bytes)
    }
}
