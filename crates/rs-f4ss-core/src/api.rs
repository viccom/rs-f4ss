//! REST API routes for mount and share management.
//!
//! Requires `feature = "api"`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::manager::{self, MountEntry, MountManager};

#[cfg(feature = "serve")]
use crate::share_manager::{ShareConfig, ShareManager};

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub mounts: MountManager,
    #[cfg(feature = "serve")]
    pub shares: ShareManager,
}

#[cfg(feature = "serve")]
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui_page))
        .route("/vue.js", get(vue_js))
        .route("/api/health", get(health))
        .route("/api/version", get(version))
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
        .with_state(state)
}

#[cfg(not(feature = "serve"))]
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(ui_page))
        .route("/vue.js", get(vue_js))
        .route("/api/health", get(health))
        .route("/api/version", get(version))
        .route("/api/mounts", get(list_mounts).post(create_mount))
        .route(
            "/api/mounts/{id}",
            get(get_mount).put(update_mount).delete(delete_mount),
        )
        .route("/api/mounts/{id}/start", post(start_mount))
        .route("/api/mounts/{id}/stop", post(stop_mount))
        .with_state(state)
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
    let config = ShareConfig {
        id: req.id,
        path: req.path,
        addr: req.addr,
        user: req.user,
        pass: req.pass,
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

    let updated = ShareConfig {
        id: id.clone(),
        path: req.path.unwrap_or(existing.path),
        addr: req.addr.unwrap_or(existing.addr),
        user: req.user.filter(|s| !s.is_empty()).or(existing.user),
        pass: req.pass.filter(|s| !s.is_empty()).or(existing.pass),
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

        // Create
        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
            .body(Body::from(test_create_body("m1")))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // List
        let req = Request::builder()
            .uri("/api/mounts")
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
            .body(Body::from(test_create_body("dup")))
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri("/api/mounts")
            .header("content-type", "application/json")
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
            .body(Body::from(test_create_body("del")))
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("DELETE")
            .uri("/api/mounts/del")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}
