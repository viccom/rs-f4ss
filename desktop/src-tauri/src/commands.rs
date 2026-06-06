use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use rs_f4ss_core::manager::{MountEntry, MountInfo, MountManager};

pub type AppMgr = Arc<MountManager>;

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    version: String,
    name: String,
}

// ── Read-only commands ──

#[tauri::command]
pub fn health() -> &'static str {
    "ok"
}

#[tauri::command]
pub fn version() -> VersionInfo {
    VersionInfo {
        version: format!("{}-{}", env!("CARGO_PKG_VERSION"), env!("GIT_HASH")),
        name: env!("CARGO_PKG_NAME").to_string(),
    }
}

#[tauri::command]
pub fn list_mounts(mgr: State<'_, AppMgr>) -> Vec<MountInfo> {
    mgr.list()
}

#[tauri::command]
pub fn get_mount(mgr: State<'_, AppMgr>, id: String) -> Result<MountInfo, String> {
    mgr.get(&id)
        .ok_or_else(|| format!("Mount {} not found", id))
}

// ── CRUD commands ──

#[derive(Debug, Deserialize)]
pub struct CreateMountReq {
    pub id: String,
    pub url: String,
    pub mountpoint: String,
    #[serde(default)]
    pub read_only: bool,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,
}

fn default_cache_ttl() -> u64 {
    60
}
fn default_cache_size() -> usize {
    256
}

#[tauri::command]
pub fn create_mount(mgr: State<'_, AppMgr>, req: CreateMountReq) -> Result<MountInfo, String> {
    let entry = MountEntry {
        id: req.id,
        url: req.url,
        mountpoint: PathBuf::from(&req.mountpoint),
        read_only: req.read_only,
        username: req.username,
        password: req.password,
        cache_ttl_secs: req.cache_ttl_secs,
        cache_size: req.cache_size,
    };
    let id = mgr.add(entry)?;
    mgr.get(&id).ok_or_else(|| "Created but not found".into())
}

#[derive(Debug, Deserialize)]
pub struct UpdateMountReq {
    pub url: Option<String>,
    pub mountpoint: Option<String>,
    pub read_only: Option<bool>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub cache_ttl_secs: Option<u64>,
    pub cache_size: Option<usize>,
}

#[tauri::command]
pub fn update_mount(
    mgr: State<'_, AppMgr>,
    id: String,
    req: UpdateMountReq,
) -> Result<MountInfo, String> {
    let existing = mgr
        .config_entries()
        .get(&id)
        .map(|r| r.value().clone())
        .ok_or_else(|| format!("Mount {} not found", id))?;

    let updated = MountEntry {
        id: id.clone(),
        url: req.url.unwrap_or(existing.url),
        mountpoint: req
            .mountpoint
            .map(PathBuf::from)
            .unwrap_or(existing.mountpoint),
        read_only: req.read_only.unwrap_or(existing.read_only),
        username: req.username.filter(|s| !s.is_empty()).or(existing.username),
        password: req.password.filter(|s| !s.is_empty()).or(existing.password),
        cache_ttl_secs: req.cache_ttl_secs.unwrap_or(existing.cache_ttl_secs),
        cache_size: req.cache_size.unwrap_or(existing.cache_size),
    };
    mgr.update(&id, updated)?;
    mgr.get(&id).ok_or_else(|| "Updated but not found".into())
}

#[tauri::command]
pub fn delete_mount(mgr: State<'_, AppMgr>, id: String) -> Result<(), String> {
    mgr.remove(&id)?;
    Ok(())
}

// ── Lifecycle commands ──

#[tauri::command]
pub fn start_mount(mgr: State<'_, AppMgr>, id: String) -> Result<MountInfo, String> {
    let entry = mgr
        .config_entries()
        .get(&id)
        .map(|r| r.value().clone())
        .ok_or_else(|| format!("Mount {} not found", id))?;

    let url = entry.url.clone();
    let read_only = entry.read_only;
    let username = entry.username.clone();
    let password = entry.password.clone();

    mgr.start_deferred(&id, move || {
        create_backend(&url, read_only, username.as_deref(), password.as_deref())
    })?;

    mgr.get(&id).ok_or_else(|| "Started but not found".into())
}

#[tauri::command]
pub fn stop_mount(mgr: State<'_, AppMgr>, id: String) -> Result<MountInfo, String> {
    mgr.stop(&id)?;
    mgr.get(&id).ok_or_else(|| "Stopped but not found".into())
}

// ── Config persistence ──

#[tauri::command]
pub fn restore_mounts(mgr: State<'_, AppMgr>) {
    mgr.restore_entries();
}

// ── Helpers ──

fn create_backend(
    url: &str,
    read_only: bool,
    username: Option<&str>,
    password: Option<&str>,
) -> Result<Box<dyn rs_f4ss_core::StorageBackend>, String> {
    let _protocol = rs_f4ss_core::detect_protocol(url);

    let backend = rs_f4ss_core::WebDavBackend::from_url(url, read_only, username, password)?;
    Ok(Box::new(backend))
}
