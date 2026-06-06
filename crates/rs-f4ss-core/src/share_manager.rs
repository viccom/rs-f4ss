//! ShareManager — dynamic file sharing configuration and lifecycle management.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::persistence;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareConfig {
    pub id: String,
    pub path: String,
    pub addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pass: Option<String>,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShareState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareInfo {
    pub id: String,
    pub path: String,
    pub addr: String,
    pub state: ShareState,
    pub read_only: bool,
    pub has_auth: bool,
}

// ---------------------------------------------------------------------------
// Internal handle for a running share
// ---------------------------------------------------------------------------

struct ShareHandle {
    state: Arc<std::sync::Mutex<ShareState>>,
    cancel: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ShareHandle {
    fn state(&self) -> ShareState {
        self.state.lock().unwrap().clone()
    }
}

// ---------------------------------------------------------------------------
// ShareManager
// ---------------------------------------------------------------------------

pub struct ShareManager {
    entries: DashMap<String, ShareConfig>,
    handles: DashMap<String, ShareHandle>,
    persist_path: Option<PathBuf>,
}

impl Default for ShareManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareManager {
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            handles: DashMap::new(),
            persist_path: None,
        }
    }

    pub fn new_with_persistence(path: PathBuf) -> Self {
        Self {
            entries: DashMap::new(),
            handles: DashMap::new(),
            persist_path: Some(path),
        }
    }

    pub fn restore_entries(&self) {
        let path = match &self.persist_path {
            Some(p) => p.clone(),
            None => return,
        };
        let entries = persistence::load_shares(&path);
        let count = entries.len();
        for entry in entries {
            let id = entry.id.clone();
            use dashmap::mapref::entry::Entry;
            match self.entries.entry(id) {
                Entry::Vacant(v) => {
                    v.insert(entry);
                }
                Entry::Occupied(_) => {}
            }
        }
        if count > 0 {
            tracing::info!("Restored {count} share config(s) from {}", path.display());
        }
    }

    fn persist(&self) {
        if let Some(ref path) = self.persist_path {
            persistence::save_shares(&self.entries, path);
        }
    }

    pub fn config_entries(&self) -> &DashMap<String, ShareConfig> {
        &self.entries
    }

    // ── CRUD ──

    pub fn add(&self, entry: ShareConfig) -> Result<String, String> {
        // Validate path exists
        let p = PathBuf::from(&entry.path);
        if !p.is_dir() {
            return Err(format!(
                "Path does not exist or is not a directory: {}",
                entry.path
            ));
        }

        let id = entry.id.clone();
        use dashmap::mapref::entry::Entry;
        match self.entries.entry(id.clone()) {
            Entry::Vacant(v) => {
                v.insert(entry);
                self.persist();
                Ok(id)
            }
            Entry::Occupied(_) => Err(format!("Share {id} already exists")),
        }
    }

    pub fn get(&self, id: &str) -> Option<ShareInfo> {
        let entry = self.entries.get(id)?;
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(ShareState::Stopped);
        Some(ShareInfo {
            id: entry.id.clone(),
            path: entry.path.clone(),
            addr: entry.addr.clone(),
            state,
            read_only: entry.read_only,
            has_auth: entry.user.is_some(),
        })
    }

    pub fn list(&self) -> Vec<ShareInfo> {
        self.entries
            .iter()
            .filter_map(|r| self.get(r.key()))
            .collect()
    }

    pub fn update(&self, id: &str, update: ShareConfig) -> Result<(), String> {
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(ShareState::Stopped);
        if !matches!(state, ShareState::Stopped | ShareState::Error(_)) {
            return Err(format!("Cannot update share {}: state is {:?}", id, state));
        }
        if id != update.id {
            return Err("Cannot change share ID".to_string());
        }
        let p = PathBuf::from(&update.path);
        if !p.is_dir() {
            return Err(format!(
                "Path does not exist or is not a directory: {}",
                update.path
            ));
        }
        self.entries.insert(id.to_string(), update);
        self.persist();
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<ShareConfig, String> {
        let state = self
            .handles
            .get(id)
            .map(|h| h.state())
            .unwrap_or(ShareState::Stopped);
        if !matches!(state, ShareState::Stopped | ShareState::Error(_)) {
            return Err(format!("Cannot remove share {}: state is {:?}", id, state));
        }
        self.handles.remove(id);
        let result = self
            .entries
            .remove(id)
            .map(|(_, e)| e)
            .ok_or_else(|| format!("Share {} not found", id));
        if result.is_ok() {
            self.persist();
        }
        result
    }

    // ── Lifecycle ──

    pub async fn start(&self, id: &str) -> Result<(), String> {
        use dashmap::mapref::entry::Entry;

        let id_owned = id.to_string();

        // Clean up stale handle
        if let Some(h) = self.handles.get(&id_owned) {
            let state = h.state();
            if matches!(state, ShareState::Stopped | ShareState::Error(_)) {
                drop(h);
                self.handles.remove(&id_owned);
            }
        }

        match self.handles.entry(id_owned.clone()) {
            Entry::Vacant(v) => {
                let entry = self
                    .entries
                    .get(id)
                    .ok_or_else(|| format!("Share {id} not found"))?
                    .clone();

                let share_state = Arc::new(std::sync::Mutex::new(ShareState::Starting));
                let cancel = Arc::new(AtomicBool::new(false));

                let root = PathBuf::from(&entry.path);
                let auth = entry
                    .user
                    .as_ref()
                    .map(|u| (u.clone(), entry.pass.clone().unwrap_or_default()));
                let read_only = entry.read_only;
                let addr = entry.addr.clone();
                let task_cancel = cancel.clone();
                let task_state = share_state.clone();
                let task_id = id_owned.clone();

                let handle = tokio::spawn(async move {
                    let config = crate::server::FileServerConfig {
                        root,
                        read_only,
                        auth,
                    };

                    *task_state.lock().unwrap() = ShareState::Running;
                    tracing::info!("Share {task_id} started on {addr}");

                    // Run the server; check cancel periodically
                    let result = tokio::select! {
                        r = crate::server::serve(config, &addr) => r,
                        _ = tokio::spawn(async move {
                            while !task_cancel.load(Ordering::Relaxed) {
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                        }) => Ok(())
                    };

                    match result {
                        Ok(()) => {
                            *task_state.lock().unwrap() = ShareState::Stopped;
                            tracing::info!("Share {task_id} stopped");
                        }
                        Err(e) => {
                            tracing::error!("Share {task_id} error: {e}");
                            *task_state.lock().unwrap() = ShareState::Error(e.to_string());
                        }
                    }
                });

                v.insert(ShareHandle {
                    state: share_state,
                    cancel,
                    task: Some(handle),
                });
                Ok(())
            }
            Entry::Occupied(o) => {
                let state = o.get().state();
                if matches!(state, ShareState::Stopped | ShareState::Error(_)) {
                    drop(o);
                    self.handles.remove(&id_owned);
                    Err(format!(
                        "Share {id} is {state:?}, removed old handle. Retry the request."
                    ))
                } else {
                    Err(format!("Cannot start share {id}: state is {state:?}"))
                }
            }
        }
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        let mut handle = self
            .handles
            .get_mut(id)
            .ok_or_else(|| format!("Share {} not found or not started", id))?;

        let state = handle.state();
        if state != ShareState::Running && state != ShareState::Starting {
            return Err(format!("Cannot stop share {}: state is {:?}", id, state));
        }

        *handle.state.lock().unwrap() = ShareState::Stopping;
        handle.cancel.store(true, Ordering::Release);

        if let Some(task) = handle.task.take() {
            task.abort();
        }

        *handle.state.lock().unwrap() = ShareState::Stopped;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(id: &str) -> ShareConfig {
        ShareConfig {
            id: id.to_string(),
            path: std::env::temp_dir().to_string_lossy().to_string(),
            addr: format!("127.0.0.1:{}", 18000 + id.len() as u16),
            user: None,
            pass: None,
            read_only: false,
        }
    }

    #[test]
    fn test_add_share() {
        let mgr = ShareManager::new();
        let id = mgr.add(test_config("s1")).unwrap();
        let info = mgr.get(&id).unwrap();
        assert_eq!(info.path, std::env::temp_dir().to_string_lossy().as_ref());
        assert_eq!(info.state, ShareState::Stopped);
        assert!(!info.read_only);
    }

    #[test]
    fn test_add_duplicate_rejected() {
        let mgr = ShareManager::new();
        mgr.add(test_config("s1")).unwrap();
        let err = mgr.add(test_config("s1")).unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn test_list_shares() {
        let mgr = ShareManager::new();
        mgr.add(test_config("a")).unwrap();
        mgr.add(test_config("b")).unwrap();
        let list = mgr.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_update_share() {
        let mgr = ShareManager::new();
        mgr.add(test_config("s1")).unwrap();

        let mut updated = test_config("s1");
        updated.read_only = true;
        mgr.update("s1", updated).unwrap();

        let info = mgr.get("s1").unwrap();
        assert!(info.read_only);
    }

    #[test]
    fn test_update_running_rejected() {
        let mgr = ShareManager::new();
        mgr.add(test_config("s1")).unwrap();
        mgr.handles.insert(
            "s1".to_string(),
            ShareHandle {
                state: Arc::new(std::sync::Mutex::new(ShareState::Running)),
                cancel: Arc::new(AtomicBool::new(false)),
                task: None,
            },
        );
        let err = mgr.update("s1", test_config("s1")).unwrap_err();
        assert!(err.contains("Cannot update"));
    }

    #[test]
    fn test_remove_share() {
        let mgr = ShareManager::new();
        mgr.add(test_config("s1")).unwrap();
        let removed = mgr.remove("s1").unwrap();
        assert_eq!(removed.id, "s1");
        assert!(mgr.get("s1").is_none());
    }

    #[test]
    fn test_remove_not_found() {
        let mgr = ShareManager::new();
        let err = mgr.remove("nonexistent").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_stop_not_started() {
        let mgr = ShareManager::new();
        mgr.add(test_config("s1")).unwrap();
        let err = mgr.stop("s1").unwrap_err();
        assert!(err.contains("not found or not started"));
    }

    #[test]
    fn test_add_invalid_path() {
        let mgr = ShareManager::new();
        let mut cfg = test_config("bad");
        cfg.path = "/nonexistent/path/xyz".to_string();
        let err = mgr.add(cfg).unwrap_err();
        assert!(err.contains("does not exist"));
    }
}
