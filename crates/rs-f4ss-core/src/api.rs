//! REST API routes for mount and share management.
//!
//! Requires `feature = "api"`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;

use crate::manager::{self, MountEntry, MountManager};
use crate::persistence::{self, AuthConfig};

#[cfg(feature = "serve")]
use crate::share_manager::{ShareConfig, ShareManager};

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub mounts: MountManager,
    #[cfg(feature = "serve")]
    pub shares: ShareManager,
    pub auth: Mutex<AuthConfig>,
    pub persist_path: PathBuf,
}

impl AppState {
    pub fn check_basic_auth(&self, headers: &axum::http::HeaderMap) -> bool {
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
            return false;
        };

        let decoded = match STANDARD.decode(encoded) {
            Ok(d) => d,
            Err(_) => return false,
        };
        let credentials = match String::from_utf8(decoded) {
            Ok(s) => s,
            Err(_) => return false,
        };
        let Some((user, pass)) = credentials.split_once(':') else {
            return false;
        };

        let auth = self.auth.lock().unwrap();
        let incoming_hash = persistence::sha256_hex(pass);
        user == auth.username && incoming_hash == auth.password_hash
    }
}

// ---------------------------------------------------------------------------
// Auth middleware
// ---------------------------------------------------------------------------

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let path = req.uri().path();
    if matches!(
        path,
        "/api/health" | "/api/version" | "/" | "/vue.js" | "/api/auth/login"
    ) {
        return next.run(req).await;
    }

    if state.check_basic_auth(req.headers()) {
        return next.run(req).await;
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(error_json("Unauthorized: invalid credentials")),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Auth endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    let hash = persistence::sha256_hex(&req.password);
    let auth = state.auth.lock().unwrap();
    if req.username == auth.username && hash == auth.password_hash {
        Json(serde_json::json!({"username": auth.username})).into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(error_json("Invalid username or password")),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    old_password: String,
    new_password: String,
}

async fn auth_change_password(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChangePasswordRequest>,
) -> impl IntoResponse {
    // Validate Basic Auth header first, then persist to disk
    // before updating in-memory state to avoid divergence on failure.
    let new_hash = persistence::sha256_hex(&req.new_password);
    {
        let mut auth = state.auth.lock().unwrap();

        // Verify Basic Auth from header (case-insensitive per RFC 7617)
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
            return (StatusCode::UNAUTHORIZED, Json(error_json("Unauthorized"))).into_response();
        };
        let decoded = match STANDARD.decode(encoded) {
            Ok(d) => d,
            Err(_) => {
                return (StatusCode::UNAUTHORIZED, Json(error_json("Unauthorized"))).into_response()
            }
        };
        let credentials = match String::from_utf8(decoded) {
            Ok(s) => s,
            Err(_) => {
                return (StatusCode::UNAUTHORIZED, Json(error_json("Unauthorized"))).into_response()
            }
        };
        let Some((user, pass)) = credentials.split_once(':') else {
            return (StatusCode::UNAUTHORIZED, Json(error_json("Unauthorized"))).into_response();
        };
        let header_hash = persistence::sha256_hex(pass);
        if user != auth.username || header_hash != auth.password_hash {
            return (StatusCode::UNAUTHORIZED, Json(error_json("Unauthorized"))).into_response();
        }

        // Verify old_password from body
        let old_hash = persistence::sha256_hex(&req.old_password);
        if old_hash != auth.password_hash {
            return (
                StatusCode::UNAUTHORIZED,
                Json(error_json("Old password is incorrect")),
            )
                .into_response();
        }

        // Business validation (after auth to avoid leaking state)
        if req.new_password.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_json("Password cannot be empty")),
            )
                .into_response();
        }

        if req.old_password == req.new_password {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_json("New password must be different")),
            )
                .into_response();
        }

        // Persist to disk first (uses store_lock, not auth Mutex — no deadlock),
        // then update in-memory state, all while holding auth Mutex to prevent races.
        let updated = AuthConfig {
            username: auth.username.clone(),
            password_hash: new_hash.clone(),
        };
        if let Err(e) = persistence::save_auth(&updated, &state.persist_path) {
            tracing::error!("Failed to persist password change: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(error_json("Failed to save password change")),
            )
                .into_response();
        }
        auth.password_hash = new_hash;
    }

    Json(serde_json::json!({"message": "Password changed"})).into_response()
}

#[cfg(feature = "serve")]
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui_page))
        .route("/vue.js", get(vue_js))
        .route("/api/health", get(health))
        .route("/api/version", get(version))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/password", post(auth_change_password))
        .route("/api/mounts", get(list_mounts).post(create_mount))
        .route(
            "/api/mounts/{id}",
            get(get_mount).put(update_mount).delete(delete_mount),
        )
        .route("/api/mounts/{id}/start", post(start_mount))
        .route("/api/mounts/{id}/stop", post(stop_mount))
        .route("/api/shares", get(list_shares).post(create_share))
        .route(
            "/api/shares/{id}",
            get(get_share).put(update_share).delete(delete_share),
        )
        .route("/api/shares/{id}/start", post(start_share))
        .route("/api/shares/{id}/stop", post(stop_share))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth_middleware))
}

#[cfg(not(feature = "serve"))]
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui_page))
        .route("/vue.js", get(vue_js))
        .route("/api/health", get(health))
        .route("/api/version", get(version))
        .route("/api/auth/login", post(auth_login))
        .route("/api/auth/password", post(auth_change_password))
        .route("/api/mounts", get(list_mounts).post(create_mount))
        .route(
            "/api/mounts/{id}",
            get(get_mount).put(update_mount).delete(delete_mount),
        )
        .route("/api/mounts/{id}/start", post(start_mount))
        .route("/api/mounts/{id}/stop", post(stop_mount))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, auth_middleware))
}

// ---------------------------------------------------------------------------
// Web UI
// ---------------------------------------------------------------------------

async fn ui_page() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("ui.html"),
    )
}

async fn vue_js() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "public, max-age=604800"),
        ],
        include_str!("vue.js"),
    )
}

// ---------------------------------------------------------------------------
// Health & Version
// ---------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn version() -> impl IntoResponse {
    Json(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "name": env!("CARGO_PKG_NAME"),
    }))
}

// ---------------------------------------------------------------------------
// Mount CRUD
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateMountRequest {
    pub id: String,
    pub url: String,
    pub mountpoint: String,
    #[serde(default)]
    pub read_only: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "manager::default_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "manager::default_cache_size")]
    pub cache_size: usize,
}

async fn create_mount(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateMountRequest>,
) -> impl IntoResponse {
    let entry = MountEntry {
        id: req.id,
        url: req.url,
        mountpoint: std::path::PathBuf::from(&req.mountpoint),
        read_only: req.read_only,
        username: req.username,
        password: req.password,
        cache_ttl_secs: req.cache_ttl_secs,
        cache_size: req.cache_size,
    };
    match state.mounts.add(entry) {
        Ok(id) => match state.mounts.get(&id) {
            Some(info) => (StatusCode::CREATED, Json(info)).into_response(),
            None => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

async fn list_mounts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.mounts.list())
}

async fn get_mount(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.mounts.get(&id) {
        Some(info) => Json(info).into_response(),
        None => (StatusCode::NOT_FOUND, Json(error_json("Mount not found"))).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateMountRequest {
    pub id: Option<String>,
    pub url: Option<String>,
    pub mountpoint: Option<String>,
    pub read_only: Option<bool>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub cache_ttl_secs: Option<u64>,
    pub cache_size: Option<usize>,
}

async fn update_mount(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateMountRequest>,
) -> impl IntoResponse {
    if let Some(ref req_id) = req.id {
        if req_id != &id {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_json("Cannot change mount ID")),
            )
                .into_response();
        }
    }

    let existing = match state.mounts.config_entries().get(&id) {
        Some(e) => e.clone(),
        None => {
            return (StatusCode::NOT_FOUND, Json(error_json("Mount not found"))).into_response()
        }
    };

    let updated = MountEntry {
        id: id.clone(),
        url: req.url.unwrap_or(existing.url),
        mountpoint: req
            .mountpoint
            .map(std::path::PathBuf::from)
            .unwrap_or(existing.mountpoint),
        read_only: req.read_only.unwrap_or(existing.read_only),
        username: req.username.filter(|s| !s.is_empty()).or(existing.username),
        password: req.password.filter(|s| !s.is_empty()).or(existing.password),
        cache_ttl_secs: req.cache_ttl_secs.unwrap_or(existing.cache_ttl_secs),
        cache_size: req.cache_size.unwrap_or(existing.cache_size),
    };

    match state.mounts.update(&id, updated) {
        Ok(()) => match state.mounts.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "updated": true })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

async fn delete_mount(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.mounts.remove(&id) {
        Ok(_) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Mount Lifecycle
// ---------------------------------------------------------------------------

async fn start_mount(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let entry = match state.mounts.config_entries().get(&id) {
        Some(e) => e.clone(),
        None => {
            return (StatusCode::NOT_FOUND, Json(error_json("Mount not found"))).into_response()
        }
    };

    if entry.mountpoint.as_os_str().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_json("Mountpoint path is empty")),
        )
            .into_response();
    }

    // Reject unsupported protocols synchronously so the API caller sees 400
    // immediately instead of a 200 with a deferred mount error.
    if !is_supported_protocol(&entry.url) {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_json(&format!("Unsupported protocol: {}", entry.url))),
        )
            .into_response();
    }

    // Capture entry data for deferred backend creation inside the mount thread.
    // This avoids creating a reqwest::Client on the API tokio runtime and then
    // moving it to a different runtime — the source of the Windows Instant overflow panic.
    let url = entry.url.clone();
    let read_only = entry.read_only;
    let username = entry.username.clone();
    let password = entry.password.clone();

    match state.mounts.start_deferred(&id, move || {
        create_backend(&url, read_only, username.as_deref(), password.as_deref())
    }) {
        Ok(()) => match state.mounts.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "state": "starting" })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

async fn stop_mount(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.mounts.stop(&id) {
        Ok(()) => match state.mounts.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "state": "stopped" })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Share CRUD (feature = "serve")
// ---------------------------------------------------------------------------

#[cfg(feature = "serve")]
#[derive(Debug, Deserialize)]
pub struct CreateShareRequest {
    pub id: String,
    pub path: String,
    pub addr: String,
    pub user: Option<String>,
    pub pass: Option<String>,
    #[serde(default)]
    pub read_only: bool,
}

#[cfg(feature = "serve")]
async fn create_share(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateShareRequest>,
) -> impl IntoResponse {
    let clean_user = req.user.filter(|s| !s.is_empty());
    let clean_pass = req.pass.filter(|s| !s.is_empty());
    if clean_user.is_some() != clean_pass.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_json(
                "user and pass must both be provided or both be empty",
            )),
        )
            .into_response();
    }

    let config = ShareConfig {
        id: req.id,
        path: req.path,
        addr: req.addr,
        user: clean_user,
        pass: clean_pass.map(|p| persistence::sha256_hex(&p)),
        read_only: req.read_only,
    };
    match state.shares.add(config) {
        Ok(id) => match state.shares.get(&id) {
            Some(info) => (StatusCode::CREATED, Json(info)).into_response(),
            None => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

#[cfg(feature = "serve")]
async fn list_shares(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.shares.list())
}

#[cfg(feature = "serve")]
async fn get_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.shares.get(&id) {
        Some(info) => Json(info).into_response(),
        None => (StatusCode::NOT_FOUND, Json(error_json("Share not found"))).into_response(),
    }
}

#[cfg(feature = "serve")]
#[derive(Debug, Deserialize)]
pub struct UpdateShareRequest {
    pub id: Option<String>,
    pub path: Option<String>,
    pub addr: Option<String>,
    pub user: Option<String>,
    pub pass: Option<String>,
    pub read_only: Option<bool>,
}

#[cfg(feature = "serve")]
async fn update_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<UpdateShareRequest>,
) -> impl IntoResponse {
    if let Some(ref req_id) = req.id {
        if req_id != &id {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_json("Cannot change share ID")),
            )
                .into_response();
        }
    }

    let existing = match state.shares.config_entries().get(&id) {
        Some(e) => e.clone(),
        None => {
            return (StatusCode::NOT_FOUND, Json(error_json("Share not found"))).into_response()
        }
    };

    let new_user = req.user.filter(|s| !s.is_empty()).or(existing.user);
    let new_pass = if new_user.is_none() {
        None
    } else {
        req.pass
            .filter(|s| !s.is_empty())
            .map(|p| persistence::sha256_hex(&p))
            .or_else(|| existing.pass.clone())
    };

    if new_user.is_some() != new_pass.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(error_json(
                "user and pass must both be provided or both be empty",
            )),
        )
            .into_response();
    }

    let updated = ShareConfig {
        id: id.clone(),
        path: req.path.unwrap_or(existing.path),
        addr: req.addr.unwrap_or(existing.addr),
        user: new_user,
        pass: new_pass,
        read_only: req.read_only.unwrap_or(existing.read_only),
    };

    match state.shares.update(&id, updated) {
        Ok(()) => match state.shares.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "updated": true })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

#[cfg(feature = "serve")]
async fn delete_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.shares.remove(&id) {
        Ok(_) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

#[cfg(feature = "serve")]
async fn start_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.shares.start(&id).await {
        Ok(()) => match state.shares.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "state": "starting" })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

#[cfg(feature = "serve")]
async fn stop_share(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.shares.stop(&id) {
        Ok(()) => match state.shares.get(&id) {
            Some(info) => Json(info).into_response(),
            None => (
                StatusCode::OK,
                Json(serde_json::json!({ "id": id, "state": "stopped" })),
            )
                .into_response(),
        },
        Err(e) => (StatusCode::CONFLICT, Json(error_json(&e))).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn error_json(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}

fn create_backend(
    url: &str,
    read_only: bool,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Box<dyn crate::backend::StorageBackend>, String> {
    let protocol = crate::backend::detect_protocol(url);

    #[cfg(feature = "webdav")]
    if protocol == "webdav" {
        let backend = crate::backend::WebDavBackend::from_url(url, read_only, username, password)?;
        return Ok(Box::new(backend));
    }

    #[cfg(feature = "http")]
    if protocol == "http" || protocol == "webdav" {
        let backend = crate::backend::HttpBackend::from_url(url, read_only, username, password)?;
        return Ok(Box::new(backend));
    }

    Err(format!("Unsupported protocol: {url}"))
}

/// True iff `url` resolves to a backend feature that is compiled in.
fn is_supported_protocol(url: &str) -> bool {
    let protocol = crate::backend::detect_protocol(url);

    #[cfg(feature = "webdav")]
    if protocol == "webdav" {
        return true;
    }

    #[cfg(feature = "http")]
    if protocol == "http" || protocol == "webdav" {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::MountInfo;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_app() -> Router {
        let state = Arc::new(AppState {
            mounts: MountManager::new(),
            #[cfg(feature = "serve")]
            shares: ShareManager::new(),
            auth: Mutex::new(AuthConfig::default()),
            persist_path: PathBuf::from("/tmp/nonexistent-test-config"),
        });
        create_router(state)
    }

    fn test_create_body(id: &str) -> String {
        serde_json::json!({
            "id": id,
            "url": "http://localhost:9000",
            "mountpoint": format!("/mnt/{id}"),
        })
        .to_string()
    }

    fn auth_header() -> String {
        basic_auth_header("admin", "admin")
    }

    #[tokio::test]
    async fn test_health() {
        let app = test_app();
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_version() {
        let app = test_app();
        let req = Request::builder()
            .uri("/api/version")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_create_and_list_mounts() {
        let app = test_app();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
            .header("authorization", auth_header())
            .body(Body::from(test_create_body("m1")))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let req = Request::builder()
            .uri("/api/mounts")
            .header("authorization", auth_header())
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let list: Vec<MountInfo> = serde_json::from_slice(&body).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "m1");
    }

    #[tokio::test]
    async fn test_create_duplicate() {
        let app = test_app();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
            .header("authorization", auth_header())
            .body(Body::from(test_create_body("dup")))
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
            .header("authorization", auth_header())
            .body(Body::from(test_create_body("dup")))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_get_mount_not_found() {
        let app = test_app();
        let req = Request::builder()
            .uri("/api/mounts/nonexistent")
            .header("authorization", auth_header())
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_mount() {
        let app = test_app();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
            .header("authorization", auth_header())
            .body(Body::from(test_create_body("del")))
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/mounts/del")
            .header("authorization", auth_header())
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    fn test_app_with_creds(user: &str, pass: &str) -> Router {
        let auth = AuthConfig {
            username: user.to_string(),
            password_hash: crate::persistence::sha256_hex(pass),
        };
        let state = Arc::new(AppState {
            mounts: MountManager::new(),
            #[cfg(feature = "serve")]
            shares: ShareManager::new(),
            auth: Mutex::new(auth),
            persist_path: PathBuf::from("/tmp/nonexistent-test-config"),
        });
        create_router(state)
    }

    fn basic_auth_header(user: &str, pass: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{user}:{pass}")))
    }

    #[tokio::test]
    async fn test_auth_valid_basic() {
        let app = test_app_with_creds("admin", "admin");
        let req = Request::builder()
            .uri("/api/mounts")
            .header("authorization", basic_auth_header("admin", "admin"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_auth_wrong_password() {
        let app = test_app_with_creds("admin", "admin");
        let req = Request::builder()
            .uri("/api/mounts")
            .header("authorization", basic_auth_header("admin", "wrong"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_missing_header() {
        let app = test_app_with_creds("admin", "admin");
        let req = Request::builder()
            .uri("/api/mounts")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_auth_health_and_login_skipped() {
        let app = test_app_with_creds("admin", "admin");
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let app2 = test_app_with_creds("admin", "admin");
        let req2 = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"username":"admin","password":"admin"}"#))
            .unwrap();
        let resp2 = app2.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
    }
}
