//! Serves the bundled frontend assets (Prism syntax highlighter, marked
//! markdown parser, DOMPurify sanitizer, Prism theme) at a reserved
//! `/_static/` path. The HTML viewer lazy-loads them on first text/markdown
//! preview, so the directory listing itself stays light and asset fetches
//! are cacheable across files.
//!
//! Files are inlined at compile time via `include_str!` — the binary is
//! self-contained and the assets are served from memory with a long
//! `max-age` so subsequent previews skip the network round-trip.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

const PRISM_JS: &str = include_str!("static/prism.js");
const MARKED_JS: &str = include_str!("static/marked.js");
const PURIFY_JS: &str = include_str!("static/purify.min.js");
const PRISM_CSS: &str = include_str!("static/prism.css");

// Long cache: these are immutable per-binary. The browser revalidates when
// the binary itself changes (since the served URL stays the same, we rely on
// `etag`-style behaviour; here we use `max-age=86400` so reloads are free).
const CACHE_IMMUTABLE: &str = "public, max-age=86400";

pub async fn serve_static(path: &str) -> Response {
    let (body, content_type) = match path {
        "prism.js" => (PRISM_JS, "application/javascript; charset=utf-8"),
        "marked.js" => (MARKED_JS, "application/javascript; charset=utf-8"),
        "purify.min.js" => (PURIFY_JS, "application/javascript; charset=utf-8"),
        "prism.css" => (PRISM_CSS, "text/css; charset=utf-8"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut resp = (StatusCode::OK, body).into_response();
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_IMMUTABLE),
    );
    // nosniff stops the browser from re-interpreting the JS as HTML if a
    // misconfigured proxy strips our Content-Type.
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    resp
}
