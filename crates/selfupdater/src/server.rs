use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::updater::Updater;

#[derive(Clone)]
struct ServerState {
    updater: Arc<Updater>,
    auth_token: Option<String>,
}

/// Build an axum Router exposing the self-update REST API.
///
/// | Endpoint       | Method | Auth | Description                              |
/// |----------------|--------|------|------------------------------------------|
/// | /api/version   | GET    | no   | Current version, platform, exe path      |
/// | /api/check     | GET    | no   | Check for newer release                  |
/// | /api/update    | POST   | yes* | Async update + restart (poll /progress)  |
/// | /api/progress  | GET    | no   | Download progress snapshot               |
///
/// *Auth required only if `auth_token` is provided. **Production servers
/// MUST pass `Some(token)`**: with `None`, any client that can reach the
/// listener can trigger a binary replacement.
pub fn router(updater: Arc<Updater>, auth_token: Option<String>) -> Router {
    if auth_token.is_none() {
        tracing::warn!(
            "selfupdater::server: /api/update is unauthenticated — any client \
             that can reach this server can trigger a binary replacement. \
             Pass Some(token) to router() to require a bearer token."
        );
    }
    let state = ServerState {
        updater,
        auth_token,
    };
    Router::new()
        .route("/api/version", get(handle_version))
        .route("/api/check", get(handle_check))
        .route("/api/update", post(handle_update))
        .route("/api/progress", get(handle_progress))
        .with_state(state)
}

async fn handle_version(State(state): State<ServerState>) -> Json<Value> {
    let exe_path = crate::replace::current_exe_resolved()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Json(json!({
        "version": state.updater.current_version(),
        "platform": crate::Release::current_platform(),
        "exe_path": exe_path,
    }))
}

async fn handle_check(State(state): State<ServerState>) -> impl IntoResponse {
    // Check uses reqwest::blocking internally; running it on a tokio worker
    // would freeze the runtime for the full request lifetime (up to the
    // 30 s client timeout). spawn_blocking moves it to a dedicated OS
    // thread so /api/version, /api/progress, /api/update stay responsive.
    let updater = state.updater.clone();
    let result = tokio::task::spawn_blocking(move || updater.check()).await;
    let result: Result<Option<crate::Release>, String> = match result {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(e) => Err(format!("internal join error: {}", e)),
    };
    match result {
        Ok(None) => Json(json!({
            "current": state.updater.current_version(),
            "has_update": false,
            "latest": state.updater.current_version(),
        })),
        Ok(Some(release)) => {
            let mut resp = json!({
                "current": state.updater.current_version(),
                "has_update": true,
                "latest": release.version,
                "date": release.date,
            });
            match release.asset_for_current_platform() {
                Ok(asset) => {
                    resp["asset"] = json!({
                        "url": asset.url,
                        "sha256": asset.sha256,
                        "size": asset.size,
                    });
                }
                Err(e) => {
                    resp["asset_error"] = json!(e.to_string());
                }
            }
            Json(resp)
        }
        Err(message) => Json(json!({
            "error": true,
            "message": message,
            "current": state.updater.current_version(),
        })),
    }
}

async fn handle_update(State(state): State<ServerState>, headers: HeaderMap) -> impl IntoResponse {
    // Check auth
    if let Some(ref token) = state.auth_token {
        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !constant_time_eq(provided.as_bytes(), token.as_bytes()) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": true, "message": "unauthorized"})),
            );
        }
    }

    // Run the blocking update on a dedicated OS thread, not a tokio worker:
    //  - reqwest::blocking + multi-MB download would otherwise block the
    //    runtime's worker pool and freeze every other endpoint.
    //  - The eventual exec() (Unix) / process::exit() (Windows) called by
    //    update_and_restart tears down the whole process (and tokio with
    //    it). This is intentional: we are replacing the binary. Any state
    //    in other runtime threads (open FDs, async tasks, unflushed logs)
    //    is lost at that point.
    let updater = state.updater.clone();
    let progress = updater.progress();
    std::thread::spawn(move || match updater.check() {
        Ok(None) => {
            tracing::info!("already up to date");
        }
        Ok(Some(release)) => {
            if let Err(e) = updater.update_and_restart(&release) {
                tracing::error!("update failed: {}", e);
                progress.set_error(&e.to_string());
            }
        }
        Err(e) => {
            tracing::error!("check failed: {}", e);
            progress.set_error(&e.to_string());
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({"accepted": true, "message": "update started"})),
    )
}

async fn handle_progress(State(state): State<ServerState>) -> Json<Value> {
    let snap = state.updater.progress().snapshot();
    Json(serde_json::to_value(snap).unwrap_or(json!({"error": "failed to serialize"})))
}

/// Constant-time comparison to prevent timing attacks.
/// Always iterates over all bytes to avoid leaking length information.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let len = a.len().max(b.len());
    let mut result = a.len() ^ b.len(); // non-zero if lengths differ
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        result |= (x ^ y) as usize;
    }
    result == 0
}
